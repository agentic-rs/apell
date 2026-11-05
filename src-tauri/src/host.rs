use crate::{provider_commands::ProviderJobs, settings::Settings};
use apell_core::{
  bundles::{BundleStore, Manifest},
  config::ConfigStore,
  filesystem::FileStore,
  http_request::HttpService,
  llm::{LlmConfig, LlmService},
  resources::ResourceStore,
  sqlite::SqliteService,
};
use serde_json::{json, Value};
use std::{
  collections::HashMap,
  path::PathBuf,
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex, RwLock,
  },
};
use uuid::Uuid;

#[derive(Clone)]
pub struct Session {
  pub manifest: Manifest,
  pub alive: Arc<AtomicBool>,
  pub cancellation: tokio::sync::watch::Sender<bool>,
  pub operations: Arc<RwLock<()>>,
}

impl Session {
  pub fn run_local<T>(&self, operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let _guard = self
      .operations
      .read()
      .map_err(|_| "Applet session is unavailable.")?;
    if !self.alive.load(Ordering::SeqCst) {
      return Err("Applet session is closed.".into());
    }
    operation()
  }
}

pub struct Host {
  pub host_key: String,
  pub bundles: Arc<BundleStore>,
  pub sqlite: Arc<SqliteService>,
  pub resources: Arc<ResourceStore>,
  pub llm: Arc<LlmService>,
  pub http: Arc<HttpService>,
  pub websocket: Arc<apell_core::websocket::WebSocketService>,
  pub files: Arc<FileStore>,
  pub config: Arc<ConfigStore>,
  pub settings: tokio::sync::Mutex<Settings>,
  pub credential_gate: tokio::sync::Mutex<()>,
  pub provider_http: apell_core::providers::Client,
  pub provider_jobs: Arc<ProviderJobs>,
  pub sessions: Mutex<HashMap<String, Session>>,
  pub running: Mutex<crate::applets::RunningApplets>,
  pub run_gate: tokio::sync::Mutex<()>,
  pub view_gate: Mutex<()>,
  pub root: PathBuf,
}

impl Host {
  pub fn new(root: PathBuf) -> Result<Self, String> {
    std::fs::create_dir_all(&root).map_err(|_| "Could not create host storage.")?;
    let settings = Settings::load(&root)?;
    let llm = Arc::new(LlmService::new()?);
    llm.register_session("shell-settings")?;
    Ok(Self {
      host_key: format!("{}{}", Uuid::new_v4(), Uuid::new_v4()),
      bundles: Arc::new(BundleStore::new(root.join("bundles"))),
      sqlite: Arc::new(SqliteService::new(root.join("databases"))),
      resources: Arc::new(ResourceStore::new(root.join("resources"))?),
      llm,
      http: Arc::new(HttpService::new()?),
      websocket: Arc::new(apell_core::websocket::WebSocketService::new()),
      files: Arc::new(FileStore::new(root.join("files"))),
      config: Arc::new(ConfigStore::new(root.join("config"))),
      settings: tokio::sync::Mutex::new(settings),
      credential_gate: tokio::sync::Mutex::new(()),
      provider_http: apell_core::providers::http_client()?,
      provider_jobs: Arc::new(ProviderJobs::default()),
      sessions: Mutex::new(HashMap::new()),
      running: Mutex::new(crate::applets::RunningApplets::default()),
      run_gate: tokio::sync::Mutex::new(()),
      view_gate: Mutex::new(()),
      root,
    })
  }

  pub fn authorize(&self, host_key: &str) -> Result<(), String> {
    if host_key != self.host_key {
      return Err("Host authorization required.".into());
    }
    Ok(())
  }

  pub fn session(&self, session_id: &str, method: &str) -> Result<Session, String> {
    let sessions = self
      .sessions
      .lock()
      .map_err(|_| "Session state is unavailable.")?;
    let session = sessions
      .get(session_id)
      .ok_or("Applet session is closed.")?;
    let capability = method.split('.').next().unwrap_or("");
    if !session.manifest.permissions.iter().any(|p| p == capability) {
      return Err("Applet did not declare this permission.".into());
    }
    Ok(session.clone())
  }

  pub fn close(&self, session_id: &str) -> Result<(), String> {
    if let Some(session) = self
      .sessions
      .lock()
      .map_err(|_| "Session state is unavailable.")?
      .remove(session_id)
    {
      // Finish an already-started local mutation before acknowledging close.
      // Queued operations then observe the closed session before touching disk.
      let _guard = session
        .operations
        .write()
        .map_err(|_| "Applet session is unavailable.")?;
      session.alive.store(false, Ordering::SeqCst);
      session.cancellation.send_replace(true);
    }
    self.llm.close_session(session_id);
    self.sqlite.close_session(session_id);
    self.websocket.close_session(session_id);
    Ok(())
  }
}

pub fn string<'a>(params: &'a Value, field: &str) -> Result<&'a str, String> {
  params
    .get(field)
    .and_then(Value::as_str)
    .ok_or_else(|| format!("Missing {field}."))
}

pub fn public_config(config: &LlmConfig) -> Value {
  let provider_name = apell_core::providers::presets()
    .into_iter()
    .find(|preset| preset.provider_id == config.provider_id)
    .map(|preset| preset.name)
    .unwrap_or_else(|| "Unknown provider".into());
  json!({
    "provider_id": config.provider_id,
    "provider_name": provider_name,
    "model_id": config.model_id,
    "connection_id": config.connection_id,
    "mode": if config.base_url == "demo" { "demo" } else { "provider" },
    "capabilities": config.capabilities(),
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{
    sync::mpsc,
    time::{Duration, Instant},
  };

  fn fixture() -> (tempfile::TempDir, Host) {
    let directory = tempfile::tempdir().unwrap();
    let host = Host::new(directory.path().to_owned()).unwrap();
    (directory, host)
  }

  #[test]
  fn active_configuration_exposes_no_connection_administration() {
    let (_directory, host) = fixture();
    let configuration = host.settings.try_lock().unwrap().active().config();
    let public = public_config(&configuration);
    assert_eq!(public["connection_id"], "local-demo");
    assert_eq!(public["mode"], "demo");
    assert_eq!(public["provider_id"], "demo");
    assert_eq!(public["provider_name"], "Local demo");
    for private in [
      "base_url",
      "name",
      "credential",
      "credential_status",
      "connections",
    ] {
      assert!(public.get(private).is_none());
    }
  }

  fn register(host: &Host, session_id: &str, permissions: &[&str]) -> Session {
    let session = Session {
      manifest: Manifest {
        schema_version: 1,
        api_version: 1,
        applet_id: "fixture".into(),
        name: "Fixture".into(),
        version: "1".into(),
        lib: None,
        permissions: permissions
          .iter()
          .map(|value| (*value).to_owned())
          .collect(),
      },
      alive: Arc::new(AtomicBool::new(true)),
      cancellation: tokio::sync::watch::channel(false).0,
      operations: Arc::new(RwLock::new(())),
    };
    host.sqlite.register_session(session_id, "fixture").unwrap();
    host.llm.register_session(session_id).unwrap();
    host
      .sessions
      .lock()
      .unwrap()
      .insert(session_id.into(), session.clone());
    session
  }

  #[test]
  fn service_permissions_are_explicit_and_do_not_cross_groups() {
    let (_directory, host) = fixture();
    register(&host, "unprivileged", &[]);
    let groups: &[(&str, &[&str])] = &[
      ("llm", &["llm.config", "llm.generate", "llm.cancel"]),
      ("http", &["http.request"]),
      (
        "fs",
        &[
          "fs.read",
          "fs.write",
          "fs.list",
          "fs.stat",
          "fs.mkdir",
          "fs.remove",
          "fs.rename",
        ],
      ),
      ("config", &["config.get", "config.set", "config.delete"]),
    ];
    for (group, methods) in groups {
      register(&host, group, &[*group]);
      for method in *methods {
        assert_eq!(
          host.session("unprivileged", method).err().unwrap(),
          "Applet did not declare this permission."
        );
        assert_eq!(
          host.session(group, method).unwrap().manifest.applet_id,
          "fixture"
        );
        assert_eq!(
          host.session("missing", method).err().unwrap(),
          "Applet session is closed."
        );
        for (other_group, _) in groups {
          if other_group != group && host.sessions.lock().unwrap().contains_key(*other_group) {
            assert!(host.session(other_group, method).is_err());
          }
        }
      }
      assert!(host
        .session(group, &format!("{group}_other.request"))
        .is_err());
    }
    // Check the complete cross-product after all three sessions are registered.
    for (owner, _) in groups {
      for (group, methods) in groups {
        for method in *methods {
          assert_eq!(host.session(owner, method).is_ok(), owner == group);
        }
      }
    }
  }

  #[test]
  fn closing_revokes_lookup_cancels_async_work_and_rejects_stale_local_work() {
    let (_directory, host) = fixture();
    let session = register(&host, "session", &["http", "fs", "config", "sqlite", "llm"]);
    let cancellation = session.cancellation.subscribe();
    assert!(!*cancellation.borrow());
    session
      .run_local(|| {
        host.config.dispatch(
          "fixture",
          "config.set",
          json!({
            "key": "before_close", "value": true,
          }),
        )
      })
      .unwrap();

    host.close("session").unwrap();

    assert!(!session.alive.load(Ordering::SeqCst));
    assert!(cancellation.has_changed().unwrap());
    assert!(*cancellation.borrow());
    assert!(host.session("session", "fs.write").is_err());
    let executed = AtomicBool::new(false);
    assert_eq!(
      session
        .run_local(|| {
          executed.store(true, Ordering::SeqCst);
          Ok(())
        })
        .unwrap_err(),
      "Applet session is closed."
    );
    assert!(!executed.load(Ordering::SeqCst));
    assert!(host
      .sqlite
      .dispatch(
        "session",
        "fixture",
        "sqlite.open",
        json!({
          "database_name": "late_open",
        })
      )
      .is_err());
    assert!(host.llm.cancel("session", "late_request").is_err());
    assert_eq!(
      host
        .config
        .dispatch(
          "fixture",
          "config.get",
          json!({
            "key": "before_close",
          })
        )
        .unwrap(),
      json!({ "found": true, "value": true })
    );
    host.close("session").unwrap();
  }

  #[test]
  fn close_acknowledges_only_after_a_started_local_mutation_finishes() {
    let (directory, host) = fixture();
    let host = Arc::new(host);
    let session = register(&host, "session", &["fs"]);
    let completed = Arc::new(AtomicBool::new(false));
    let (entered_tx, entered_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let (closed_tx, closed_rx) = mpsc::channel();
    std::thread::scope(|scope| {
      let operation_session = session.clone();
      let operation_host = host.clone();
      let operation_completed = completed.clone();
      let operation = scope.spawn(move || {
        operation_session.run_local(|| {
          entered_tx.send(()).unwrap();
          release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
          operation_host.files.dispatch(
            "fixture",
            "fs.write",
            json!({
              "path": "completed.bin", "bytes": [1, 2, 3],
            }),
          )?;
          operation_completed.store(true, Ordering::SeqCst);
          Ok(())
        })
      });
      entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

      let closing_host = host.clone();
      let closing_completed = completed.clone();
      let closing = scope.spawn(move || {
        let result = closing_host.close("session");
        let mutation_finished = closing_completed.load(Ordering::SeqCst);
        closed_tx.send((result, mutation_finished)).unwrap();
      });

      // Observe close entering the session registry, rather than relying on a
      // sleep to guess whether the closing thread has been scheduled. Close may
      // retain the registry lock or release it after removing the session.
      let deadline = Instant::now() + Duration::from_secs(5);
      loop {
        if let Ok((_, finished)) = closed_rx.try_recv() {
          assert!(finished, "close acknowledged before the mutation finished");
          panic!("the mutation has not been released yet");
        }
        match host.sessions.try_lock() {
          Err(std::sync::TryLockError::WouldBlock) => break,
          Ok(sessions) if !sessions.contains_key("session") => break,
          Ok(_) => {}
          Err(error) => panic!("session registry is poisoned: {error}"),
        }
        assert!(
          Instant::now() < deadline,
          "close did not enter the session registry"
        );
        std::thread::yield_now();
      }
      assert!(!completed.load(Ordering::SeqCst));
      assert!(matches!(
        closed_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
      ));
      release_tx.send(()).unwrap();
      operation.join().unwrap().unwrap();
      let (result, finished) = closed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
      result.unwrap();
      assert!(finished);
      closing.join().unwrap();
    });
    assert_eq!(
      std::fs::read(directory.path().join("files/fixture/completed.bin")).unwrap(),
      [1, 2, 3]
    );
    assert!(session.run_local(|| Ok(())).is_err());
  }
}

use crate::host::{Host, Session};
use apell_core::bundles::{Bundle, Manifest};
use apell_runtime::{HostError, HostEvents, HostServices, RuntimeHandle, ServiceFuture};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
  collections::HashMap,
  sync::{atomic::AtomicBool, Arc, RwLock},
};
use tauri::{AppHandle, Emitter, Manager};

struct RunningApplet {
  bundle: Arc<Bundle>,
  backend: Option<RuntimeHandle>,
}

const BACKEND_STOPPED: &str = "apell:stopped";

#[derive(Default)]
pub struct RunningApplets {
  runs: HashMap<String, RunningApplet>,
  foreground: Option<String>,
  view: Option<(String, String)>,
}

#[derive(Clone, Serialize)]
pub struct Attachment {
  pub session_id: String,
  pub view_id: String,
  pub manifest: Manifest,
  pub entry_url: String,
  pub has_lib: bool,
}

impl Host {
  // The caller holds view_gate. View-owned service handles are released before
  // publishing the replacement attachment; backend service handles stay alive.
  fn attach_run_locked(&self, session_id: &str) -> Result<Attachment, String> {
    let (manifest, has_lib, previous) = {
      let mut running = self
        .running
        .lock()
        .map_err(|_| "Applet state is unavailable.")?;
      let run = running
        .runs
        .get(session_id)
        .ok_or("Applet backend is unavailable.")?;
      let metadata = (run.bundle.manifest.clone(), run.backend.is_some());
      (metadata.0, metadata.1, running.view.take())
    };
    if let Some((_, old_view)) = previous {
      self.close(&old_view)?;
    }
    let view_id = uuid::Uuid::new_v4().to_string();
    self
      .sqlite
      .register_session(&view_id, &manifest.applet_id)?;
    if let Err(error) = self.llm.register_session(&view_id) {
      self.sqlite.close_session(&view_id);
      return Err(error);
    }
    self
      .sessions
      .lock()
      .map_err(|_| "Session state is unavailable.")?
      .insert(
        view_id.clone(),
        Session {
          manifest: manifest.clone(),
          alive: Arc::new(AtomicBool::new(true)),
          cancellation: tokio::sync::watch::channel(false).0,
          operations: Arc::new(RwLock::new(())),
        },
      );
    let attachment = Attachment {
      session_id: session_id.into(),
      view_id: view_id.clone(),
      manifest,
      entry_url: crate::assets::entry_url(&view_id),
      has_lib,
    };
    let mut running = self
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?;
    running.foreground = Some(session_id.into());
    running.view = Some((session_id.into(), view_id));
    Ok(attachment)
  }

  fn attach_run(&self, session_id: &str) -> Result<Attachment, String> {
    let _gate = self
      .view_gate
      .lock()
      .map_err(|_| "Applet views are unavailable.")?;
    self.attach_run_locked(session_id)
  }

  pub fn require_view(&self, session_id: &str, view_id: &str) -> Result<(), String> {
    let running = self
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?;
    if running
      .view
      .as_ref()
      .is_some_and(|(run, view)| run == session_id && view == view_id)
    {
      Ok(())
    } else {
      Err("This applet view was detached. Reopen the applet.".into())
    }
  }

  pub fn attach_view(&self, session_id: &str, view_id: &str) -> Result<Attachment, String> {
    let _gate = self
      .view_gate
      .lock()
      .map_err(|_| "Applet views are unavailable.")?;
    self.require_view(session_id, view_id)?;
    self.attach_run_locked(session_id)
  }

  pub fn resume_view(&self) -> Result<Option<Attachment>, String> {
    let _gate = self
      .view_gate
      .lock()
      .map_err(|_| "Applet views are unavailable.")?;
    let running = self
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?;
    let Some(id) = running.foreground.clone() else {
      return Ok(None);
    };
    drop(running);
    self.attach_run_locked(&id).map(Some)
  }

  pub fn detach_view(&self, session_id: &str, view_id: &str) -> Result<(), String> {
    let _gate = self
      .view_gate
      .lock()
      .map_err(|_| "Applet views are unavailable.")?;
    let mut running = self
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?;
    if running
      .view
      .as_ref()
      .is_some_and(|(run, view)| run == session_id && view == view_id)
    {
      running.view = None;
      running.foreground = None;
      drop(running);
      self.close(view_id)?;
    }
    Ok(())
  }

  fn retire_run(&self, session_id: &str) -> Result<bool, String> {
    let _gate = self
      .view_gate
      .lock()
      .map_err(|_| "Applet views are unavailable.")?;
    let mut running = self
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?;
    if running.runs.remove(session_id).is_none() {
      return Ok(false);
    }
    if running.foreground.as_deref() == Some(session_id) {
      running.foreground = None;
    }
    let view = running
      .view
      .take_if(|(id, _)| id == session_id)
      .map(|(_, view)| view);
    drop(running);
    let view_closed = view.map(|id| self.close(&id)).transpose();
    self.close(session_id)?;
    view_closed?;
    Ok(true)
  }

  pub fn view_bundle(&self, view_id: &str) -> Result<Arc<Bundle>, String> {
    let running = self
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?;
    let (id, _) = running
      .view
      .as_ref()
      .filter(|(_, view)| view == view_id)
      .ok_or("Applet view is unavailable.")?;
    Ok(
      running
        .runs
        .get(id)
        .ok_or("Applet is unavailable.")?
        .bundle
        .clone(),
    )
  }

  pub async fn invoke_backend(
    &self,
    session_id: &str,
    command: &str,
    params: Value,
  ) -> Result<Value, HostError> {
    let backend = self
      .running
      .lock()
      .map_err(|_| HostError::new("Applet state is unavailable."))?
      .runs
      .get(session_id)
      .and_then(|run| run.backend.clone())
      .ok_or_else(|| HostError::new("This applet has no backend module."))?;
    backend.invoke(command, params).await
  }
}

fn retire_backend(app: &AppHandle, host: &Host, session_id: &str) -> Result<(), String> {
  if host.retire_run(session_id)? {
    let _ = app.emit_to(
      "main",
      "backend-event",
      json!({
        "session_id":session_id,
        "event":{"name":BACKEND_STOPPED,"payload":{"message":"The applet backend stopped. Reopen the applet."}}
      }),
    );
  }
  Ok(())
}

struct Services {
  app: AppHandle,
  session_id: String,
}

impl HostServices for Services {
  fn call(&self, method: String, params: Value, events: HostEvents) -> ServiceFuture {
    let app = self.app.clone();
    let session_id = self.session_id.clone();
    Box::pin(async move {
      crate::commands::service_call(app, session_id, method, params, Some(events))
        .await
        .map_err(HostError::new)
    })
  }
}

pub async fn launch(app: AppHandle, applet_id: &str) -> Result<Attachment, String> {
  let host = app.state::<Host>();
  let _gate = host.run_gate.lock().await;
  // The interpreter may have finished before its forwarding task gets the
  // gate. Reopen must never select that run or count it against the run limit.
  let stopped: Vec<_> = host
    .running
    .lock()
    .map_err(|_| "Applet state is unavailable.")?
    .runs
    .iter()
    .filter(|(_, run)| run.backend.as_ref().is_some_and(RuntimeHandle::is_stopped))
    .map(|(id, _)| id.clone())
    .collect();
  for id in stopped {
    retire_backend(&app, &host, &id)?;
  }
  {
    let running = host
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?;
    let existing = running
      .runs
      .iter()
      .find(|(_, run)| run.bundle.manifest.applet_id == applet_id)
      .map(|(id, _)| id.clone());
    if let Some(id) = existing {
      drop(running);
      return host.attach_run(&id);
    }
    if running.runs.len() >= 8 {
      return Err(
        "Eight applets are already running. Restart Apell before opening another applet.".into(),
      );
    }
  }
  let store = host.bundles.clone();
  let id = applet_id.to_owned();
  let bundle = Arc::new(
    tauri::async_runtime::spawn_blocking(move || store.load(&id))
      .await
      .map_err(|_| "Could not load the applet package.")??,
  );
  let session_id = uuid::Uuid::new_v4().to_string();
  host.sqlite.register_session(&session_id, applet_id)?;
  if let Err(error) = host.llm.register_session(&session_id) {
    host.sqlite.close_session(&session_id);
    return Err(error);
  }
  host
    .sessions
    .lock()
    .map_err(|_| "Session state is unavailable.")?
    .insert(
      session_id.clone(),
      Session {
        manifest: bundle.manifest.clone(),
        alive: Arc::new(AtomicBool::new(true)),
        cancellation: tokio::sync::watch::channel(false).0,
        operations: Arc::new(RwLock::new(())),
      },
    );
  let backend = if let Some(path) = &bundle.manifest.lib {
    let bytes = bundle
      .read_asset(path)
      .ok_or("Applet backend is missing.")?
      .to_vec();
    let start = RuntimeHandle::start(
      bytes,
      Arc::new(Services {
        app: app.clone(),
        session_id: session_id.clone(),
      }),
    )
    .await;
    match start {
      Ok(backend) => {
        let initialized = tokio::time::timeout(
          std::time::Duration::from_secs(30),
          backend.invoke("initialize", json!({})),
        )
        .await
        .map_err(|_| HostError::new("Applet initialization timed out."))
        .and_then(|result| result);
        if let Err(error) = initialized {
          backend.stop().await;
          host.close(&session_id)?;
          return Err(error.message);
        }
        let mut events = backend.subscribe();
        let emit_app = app.clone();
        let emit_id = session_id.clone();
        tauri::async_runtime::spawn(async move {
          loop {
            match events.recv().await {
              Ok(event) => {
                // Lifecycle events belong to the host, not to guest code.
                if event.get("name").and_then(Value::as_str) == Some(BACKEND_STOPPED) {
                  continue;
                }
                let _ = emit_app.emit_to(
                  "main",
                  "backend-event",
                  json!({"session_id":emit_id,"event":event}),
                );
              }
              Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
              Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                // Publish/launch holds this gate until the run and first view
                // exist. A late notification can only remove its own run ID.
                let host = emit_app.state::<Host>();
                let _gate = host.run_gate.lock().await;
                let _ = retire_backend(&emit_app, &host, &emit_id);
                break;
              }
            }
          }
        });
        Some(backend)
      }
      Err(error) => {
        host.close(&session_id)?;
        return Err(error.message);
      }
    }
  } else {
    None
  };
  let mut running = host
    .running
    .lock()
    .map_err(|_| "Applet state is unavailable.")?;
  running
    .runs
    .insert(session_id.clone(), RunningApplet { bundle, backend });
  drop(running);
  host.attach_run(&session_id)
}

#[cfg(test)]
mod tests {
  use super::*;
  use apell_core::{
    bundles::{digest, pack_directory, API_VERSION},
    llm::{LlmConfig, LlmRequest, LlmStatus},
  };
  use std::{fs, time::Duration};
  use tokio::{sync::oneshot, task::JoinHandle};

  const APPLET: &str = "view-fixture";
  const RUN: &str = "native-run";

  fn install(host: &Host, applet_id: &str, version: &str) {
    let source = tempfile::tempdir().unwrap();
    let manifest = json!({
      "schema_version": 1, "api_version": API_VERSION, "applet_id": applet_id,
      "name": "View fixture", "version": version, "permissions": ["sqlite", "llm"]
    });
    fs::write(source.path().join("manifest.json"), manifest.to_string()).unwrap();
    fs::write(
      source.path().join("index.html"),
      format!("<p>{version}</p>"),
    )
    .unwrap();
    let package = pack_directory(source.path()).unwrap();
    host.bundles.install(&package, &digest(&package)).unwrap();
  }

  fn seed_run(host: &Host, session_id: &str, applet_id: &str) {
    let bundle = Arc::new(host.bundles.load(applet_id).unwrap());
    // Seed a running H5 applet exactly as launch does before attaching a view.
    host.sqlite.register_session(session_id, applet_id).unwrap();
    host.llm.register_session(session_id).unwrap();
    host.sessions.lock().unwrap().insert(
      session_id.into(),
      Session {
        manifest: bundle.manifest.clone(),
        alive: Arc::new(AtomicBool::new(true)),
        cancellation: tokio::sync::watch::channel(false).0,
        operations: Arc::new(RwLock::new(())),
      },
    );
    host.running.lock().unwrap().runs.insert(
      session_id.into(),
      RunningApplet {
        bundle,
        backend: None,
      },
    );
  }

  fn fixture() -> (tempfile::TempDir, Host) {
    let directory = tempfile::tempdir().unwrap();
    let host = Host::new(directory.path().to_owned()).unwrap();
    install(&host, APPLET, "1.0.0");
    seed_run(&host, RUN, APPLET);
    (directory, host)
  }

  fn database(host: &Host, session: &str) -> Value {
    host
      .sqlite
      .dispatch(
        session,
        APPLET,
        "sqlite.open",
        json!({"database_name": "shared"}),
      )
      .unwrap()["database_handle"]
      .clone()
  }

  async fn generate(host: &Host, session: &str) -> JoinHandle<LlmStatus> {
    let llm = host.llm.clone();
    let session = session.to_owned();
    let request = LlmRequest {
      request_id: "generation".into(),
      messages: vec![json!({"role": "user", "content": "Hello"})],
      ..Default::default()
    };
    let (started, mut ready) = oneshot::channel();
    let started = std::sync::Mutex::new(Some(started));
    let mut work = tokio::spawn(async move {
      llm
        .generate(
          &session,
          request,
          LlmConfig::default(),
          None,
          move |event| {
            if event.kind == "started" {
              started.lock().unwrap().take().unwrap().send(()).unwrap();
            }
          },
        )
        .await
        .status
    });
    tokio::select! {
      result = &mut work => panic!("Generation ended before view rotation: {result:?}"),
      result = &mut ready => result.unwrap(),
      _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("Generation did not start"),
    }
    work
  }

  #[tokio::test]
  async fn reload_revokes_the_old_views_sqlite_and_llm_work_but_preserves_the_run() {
    let (_directory, host) = fixture();
    let first = host.attach_run(RUN).unwrap();
    assert_ne!(first.view_id, RUN);
    let old_session = host.session(&first.view_id, "sqlite.query").unwrap();
    let run_session = host.session(RUN, "sqlite.query").unwrap();
    let run_database = database(&host, RUN);
    let view_database = database(&host, &first.view_id);
    let query = |session: &str, handle: &Value| {
      host.sqlite.dispatch(
        session,
        APPLET,
        "sqlite.query",
        json!({"database_handle":handle, "sql":"SELECT 1"}),
      )
    };
    assert!(query(&first.view_id, &view_database).is_ok());
    let run_generation = generate(&host, RUN).await;
    let view_generation = generate(&host, &first.view_id).await;

    let second = host.attach_view(RUN, &first.view_id).unwrap();
    assert_eq!(second.session_id, first.session_id);
    assert_ne!(second.view_id, first.view_id);
    host.require_view(RUN, &second.view_id).unwrap();
    assert!(host.attach_view(RUN, &first.view_id).is_err());
    assert!(*old_session.cancellation.borrow());
    assert!(old_session.run_local(|| Ok(())).is_err());
    assert!(query(&first.view_id, &view_database).is_err());
    assert!(query(&second.view_id, &view_database).is_err());
    assert!(host.llm.cancel(&first.view_id, "other").is_err());
    assert_eq!(view_generation.await.unwrap(), LlmStatus::Cancelled);

    assert!(!*run_session.cancellation.borrow());
    run_session.run_local(|| Ok(())).unwrap();
    assert!(query(RUN, &run_database).is_ok());
    assert!(query(&second.view_id, &database(&host, &second.view_id)).is_ok());
    assert_eq!(run_generation.await.unwrap(), LlmStatus::Completed);
  }

  #[test]
  fn close_reopen_and_outer_resume_keep_the_run_and_its_installed_package() {
    let (_directory, host) = fixture();
    let first = host.attach_run(RUN).unwrap();
    let pinned = host.view_bundle(&first.view_id).unwrap();
    install(&host, APPLET, "2.0.0");
    assert_eq!(host.bundles.load(APPLET).unwrap().manifest.version, "2.0.0");
    assert_eq!(pinned.html, "<p>1.0.0</p>");

    host.detach_view(RUN, &first.view_id).unwrap();
    assert!(host.view_bundle(&first.view_id).is_err());
    assert!(host.session(&first.view_id, "llm.generate").is_err());
    assert!(host.resume_view().unwrap().is_none());
    host
      .session(RUN, "llm.generate")
      .unwrap()
      .run_local(|| Ok(()))
      .unwrap();

    let reopened = host.attach_run(RUN).unwrap();
    assert_eq!(reopened.session_id, first.session_id);
    assert_ne!(reopened.view_id, first.view_id);
    assert!(Arc::ptr_eq(
      &pinned,
      &host.view_bundle(&reopened.view_id).unwrap()
    ));
    // A late close from the previous view must not detach the replacement.
    host.detach_view(RUN, &first.view_id).unwrap();
    host.require_view(RUN, &reopened.view_id).unwrap();
    let resumed = host.resume_view().unwrap().unwrap();
    assert_eq!(resumed.session_id, first.session_id);
    assert_ne!(resumed.view_id, reopened.view_id);
    assert_ne!(resumed.view_id, first.view_id);
    assert!(host.require_view(RUN, &reopened.view_id).is_err());
    assert!(host.session(&reopened.view_id, "sqlite.open").is_err());
    assert!(Arc::ptr_eq(
      &pinned,
      &host.view_bundle(&resumed.view_id).unwrap()
    ));
    assert_eq!(host.running.lock().unwrap().runs.len(), 1);
    assert_eq!(host.sessions.lock().unwrap().len(), 2);
  }

  #[test]
  fn retiring_an_interpreter_revokes_only_its_run_and_view() {
    let (_directory, host) = fixture();
    let old = host.attach_run(RUN).unwrap();
    let run_session = host.session(RUN, "llm.generate").unwrap();
    let view_session = host.session(&old.view_id, "llm.generate").unwrap();
    let database_handle = database(&host, RUN);
    install(&host, "neighbor", "1.0.0");
    seed_run(&host, "neighbor-run", "neighbor");
    install(&host, "neighbor", "2.0.0");
    install(&host, APPLET, "2.0.0");

    assert!(host.retire_run(RUN).unwrap());
    assert!(*run_session.cancellation.borrow() && *view_session.cancellation.borrow());
    assert!(host.view_bundle(&old.view_id).is_err());
    assert!(host.resume_view().unwrap().is_none());
    assert!(host.llm.cancel(RUN, "request").is_err());
    assert!(host
      .sqlite
      .dispatch(
        RUN,
        APPLET,
        "sqlite.query",
        json!({"database_handle":database_handle,"sql":"SELECT 1"})
      )
      .is_err());
    let neighbor = host.attach_run("neighbor-run").unwrap();
    assert_eq!(neighbor.manifest.version, "1.0.0");

    // A reopened applet gets a new run and the newly installed snapshot.
    seed_run(&host, "reopened", APPLET);
    let reopened = host.attach_run("reopened").unwrap();
    assert_eq!(reopened.manifest.version, "2.0.0");
    assert!(!host.retire_run(RUN).unwrap());
    assert!(host.retire_run("neighbor-run").unwrap());
    host.require_view("reopened", &reopened.view_id).unwrap();
    assert_eq!(host.running.lock().unwrap().runs.len(), 1);
    assert_eq!(host.sessions.lock().unwrap().len(), 2);
  }
}

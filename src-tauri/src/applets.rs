use crate::host::{Host, Session};
use crate::permissions::{
  installed_revision, GrantSource, ManagedPermission, PermissionEditBlocker, PermissionMode,
  GRANT_CHANGED,
};
use apell_core::bundles::{Bundle, InstalledApplet, Manifest, PackageSource, StoreSource};
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
  sha256: String,
  backend: Option<RuntimeHandle>,
  grant_source: GrantSource,
}

impl RunningApplet {
  fn is_stopped(&self) -> bool {
    self.backend.as_ref().is_some_and(RuntimeHandle::is_stopped)
  }

  fn matches_install(&self, installed: &InstalledApplet) -> bool {
    self.sha256 == installed.sha256 && self.grant_source == GrantSource::from(installed)
  }
}

const BACKEND_STOPPED: &str = "apell:stopped";
const MAX_RUNNING_APPLETS: usize = 8;
const RUN_LIMIT_REACHED: &str =
  "Eight applets are already running. Stop one in Running applets before opening another.";
const RUN_CHANGED: &str =
  "The applet’s running state changed. Review Running applets and try again.";

fn require_declared_permission(
  manifest: &Manifest,
  permission: ManagedPermission,
) -> Result<(), String> {
  let group = permission.manifest_group();
  if !manifest.permissions.iter().any(|item| item == group) {
    return Err(format!("This applet did not declare {group} access."));
  }
  if permission == ManagedPermission::Camera
    && !manifest.permissions.iter().any(|item| item == "fs")
  {
    return Err("This applet must declare app files to use the camera.".into());
  }
  Ok(())
}

#[derive(Default)]
pub struct RunningApplets {
  runs: HashMap<String, RunningApplet>,
  foreground: Option<String>,
  view: Option<(String, String)>,
}

impl RunningApplets {
  fn session_for(&self, applet_id: &str) -> Option<String> {
    self
      .runs
      .iter()
      .find(|(_, run)| run.bundle.manifest.applet_id == applet_id)
      .map(|(id, _)| id.clone())
  }

  fn available_run(&self, applet_id: &str) -> Result<Option<String>, String> {
    let existing = self.session_for(applet_id);
    if existing.is_none() && self.runs.len() >= MAX_RUNNING_APPLETS {
      return Err(RUN_LIMIT_REACHED.into());
    }
    Ok(existing)
  }

  fn healthy_and_detached(&self, id: &str, run: &RunningApplet) -> bool {
    self.foreground.as_deref() != Some(id)
      && self
        .view
        .as_ref()
        .is_none_or(|(attached, _)| attached != id)
      && !run.is_stopped()
  }
}

#[derive(Clone, Serialize)]
pub struct Attachment {
  pub session_id: String,
  pub view_id: String,
  pub manifest: Manifest,
  pub entry_url: String,
  pub has_lib: bool,
}

#[derive(Serialize)]
pub struct AppletInstallation {
  installed_version: String,
  installed_sha256: String,
  store_source: Option<StoreSource>,
  restart_required: bool,
}

#[derive(Serialize)]
pub(crate) struct DetachedRun {
  session_id: String,
  applet_id: String,
  name: String,
  version: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ViewPermission {
  pub mode: PermissionMode,
  pub edit_blocker: Option<PermissionEditBlocker>,
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

  pub fn detach_view(&self, session_id: &str, view_id: &str) -> Result<bool, String> {
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
      return Ok(true);
    }
    Ok(false)
  }

  pub(crate) fn detached_runs(&self) -> Result<Vec<DetachedRun>, String> {
    let _gate = self
      .view_gate
      .lock()
      .map_err(|_| "Applet views are unavailable.")?;
    let running = self
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?;
    let mut detached: Vec<_> = running
      .runs
      .iter()
      .filter(|(id, run)| running.healthy_and_detached(id, run))
      .map(|(id, run)| {
        let manifest = &run.bundle.manifest;
        DetachedRun {
          session_id: id.clone(),
          applet_id: manifest.applet_id.clone(),
          name: manifest.name.clone(),
          version: manifest.version.clone(),
        }
      })
      .collect();
    detached
      .sort_by(|left, right| (&left.name, &left.applet_id).cmp(&(&right.name, &right.applet_id)));
    Ok(detached)
  }

  fn retire_run(
    &self,
    session_id: &str,
    expected_detached_applet: Option<&str>,
  ) -> Result<Option<RunningApplet>, String> {
    let _gate = self
      .view_gate
      .lock()
      .map_err(|_| "Applet views are unavailable.")?;
    let mut running = self
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?;
    // Verify the reviewed run and its detached state while holding the same
    // view gate that protects reattachment. A stale stop cannot affect a new run.
    if let Some(applet_id) = expected_detached_applet {
      let matches = running.runs.get(session_id).is_some_and(|run| {
        run.bundle.manifest.applet_id == applet_id && running.healthy_and_detached(session_id, run)
      });
      if !matches {
        return Err(RUN_CHANGED.into());
      }
    }
    let Some(run) = running.runs.remove(session_id) else {
      return Ok(None);
    };
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
    Ok(Some(run))
  }

  pub(crate) fn needs_applet_restart(&self, installed: &InstalledApplet) -> bool {
    self.running.lock().is_ok_and(|running| {
      running.runs.values().any(|run| {
        run.bundle.manifest.applet_id == installed.manifest.applet_id
          && !run.is_stopped()
          && !run.matches_install(installed)
      })
    })
  }

  // Callers hold run_gate until they finish stopping, removing, or replacing a run.
  async fn stop_run_locked(
    &self,
    session_id: &str,
    expected_detached_applet: Option<&str>,
    on_stopped: impl FnOnce(&str),
  ) -> Result<(), String> {
    if let Some(run) = self.retire_run(session_id, expected_detached_applet)? {
      on_stopped(session_id);
      if let Some(backend) = run.backend {
        backend.stop().await;
      }
    }
    Ok(())
  }

  async fn stop_applet_locked(
    &self,
    applet_id: &str,
    on_stopped: impl FnOnce(&str),
  ) -> Result<(), String> {
    let session_id = self
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?
      .session_for(applet_id);
    if let Some(id) = session_id {
      self.stop_run_locked(&id, None, on_stopped).await?;
    }
    Ok(())
  }

  pub(crate) async fn stop_detached_run(
    &self,
    applet_id: &str,
    session_id: &str,
    on_stopped: impl FnOnce(&str),
  ) -> Result<(), String> {
    let _gate = self.run_gate.lock().await;
    self
      .stop_run_locked(session_id, Some(applet_id), on_stopped)
      .await
  }

  pub async fn remove_applet(
    &self,
    applet_id: &str,
    on_stopped: impl FnOnce(&str),
  ) -> Result<(), String> {
    // An earlier download must finish publishing before removal. Prevent
    // launch from attaching to the retiring run or starting a replacement.
    let _install = self.store_gate.lock().await;
    let _launch = self.run_gate.lock().await;
    self.stop_applet_locked(applet_id, on_stopped).await?;
    let grants = self.grants.clone();
    let store = self.bundles.clone();
    let id = applet_id.to_owned();
    crate::commands::blocking(move || store.remove(&id, || grants.forget_all(&id))).await
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

  pub fn view_grant_source(&self, view_id: &str) -> Result<GrantSource, String> {
    let running = self
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?;
    let run = running
      .view
      .as_ref()
      .filter(|(_, view)| view == view_id)
      .and_then(|(run, _)| running.runs.get(run))
      .ok_or("Applet view is unavailable.")?;
    Ok(run.grant_source.clone())
  }

  pub fn view_installation(
    &self,
    session_id: &str,
    view_id: &str,
  ) -> Result<AppletInstallation, String> {
    // Capture the exact attached identity, even when a newer installed
    // package is waiting for the running applet to restart.
    self.require_view(session_id, view_id)?;
    let bundle = self.view_bundle(view_id)?;
    let installed = self.bundles.installed(&bundle.manifest.applet_id)?;
    // A package read can take time; don't return details to a stale view.
    self.require_view(session_id, view_id)?;
    let restart_required = self.needs_applet_restart(&installed);
    Ok(AppletInstallation {
      installed_version: installed.manifest.version,
      installed_sha256: installed.sha256,
      store_source: match installed.source {
        Some(PackageSource::Store(source)) => Some(source),
        _ => None,
      },
      restart_required,
    })
  }

  pub(crate) fn view_permission(
    &self,
    session_id: &str,
    view_id: &str,
    permission: ManagedPermission,
    update: Option<PermissionMode>,
  ) -> Result<ViewPermission, String> {
    // Keep the verified attachment current through the grant read or write.
    let _gate = self
      .view_gate
      .lock()
      .map_err(|_| "Applet views are unavailable.")?;
    let (applet_id, source) = {
      let running = self
        .running
        .lock()
        .map_err(|_| "Applet state is unavailable.")?;
      let run = running
        .view
        .as_ref()
        .filter(|(run, view)| run == session_id && view == view_id)
        .and_then(|(run, _)| running.runs.get(run))
        .ok_or("This applet view was detached. Reopen the applet.")?;
      let manifest = &run.bundle.manifest;
      require_declared_permission(manifest, permission)?;
      (manifest.applet_id.clone(), run.grant_source.clone())
    };
    let edit_blocker = self
      .check_installed_permission_target(&applet_id, &source, permission)
      .err();
    let mode = match update {
      Some(mode) => {
        if let Some(blocker) = edit_blocker {
          return Err(blocker.message().into());
        }
        let edit = || self.grants.set_mode(permission, &applet_id, &source, mode);
        match permission {
          ManagedPermission::Microphone => self.audio.edit_microphone_grant(view_id, edit),
          ManagedPermission::Camera | ManagedPermission::Location => self.sensors.edit_grant(edit),
        }
      }
      None => self.grants.mode(permission, &applet_id, &source),
    }?;
    Ok(ViewPermission { mode, edit_blocker })
  }

  pub fn remember_installed_permission(
    &self,
    permission: ManagedPermission,
    applet_id: &str,
    source: &GrantSource,
  ) -> Result<(), String> {
    // An approval still authorizes the current request. Only persist it if
    // the installed source can be checked without waiting for an install.
    let Ok(_install) = self.store_gate.try_lock() else {
      return Ok(());
    };
    if self
      .check_installed_permission_target(applet_id, source, permission)
      .is_ok()
    {
      self.grants.remember(permission, applet_id, source)?;
    }
    Ok(())
  }

  fn check_installed_permission_target(
    &self,
    applet_id: &str,
    source: &GrantSource,
    permission: ManagedPermission,
  ) -> Result<(), PermissionEditBlocker> {
    let installed = self
      .bundles
      .installed(applet_id)
      .map_err(|_| PermissionEditBlocker::InstallationUnavailable)?;
    if GrantSource::from(&installed) != *source {
      return Err(PermissionEditBlocker::SourceChanged);
    }
    require_declared_permission(&installed.manifest, permission)
      .map_err(|_| PermissionEditBlocker::PermissionRemoved)
  }

  pub fn forget_permission(
    &self,
    applet_id: &str,
    permission: ManagedPermission,
    expected_grant: &str,
  ) -> Result<(), String> {
    self.edit_detached_permission(applet_id, permission, || {
      let target = self
        .bundles
        .report_applet(applet_id)?
        .ok_or(GRANT_CHANGED)?;
      self
        .grants
        .reset_reviewed(applet_id, target, permission, expected_grant)
    })
  }

  pub fn set_installed_permission(
    &self,
    applet_id: &str,
    expected_revision: &str,
    expected_grant: &str,
    permission: ManagedPermission,
    mode: PermissionMode,
  ) -> Result<PermissionMode, String> {
    self.edit_detached_permission(applet_id, permission, || {
      let applet = self
        .bundles
        .installed(applet_id)
        .map_err(|_| GRANT_CHANGED.to_owned())?;
      let revision = installed_revision(&applet)?;
      if revision != expected_revision {
        return Err(GRANT_CHANGED.into());
      }
      require_declared_permission(&applet.manifest, permission)?;
      self
        .grants
        .set_reviewed_mode(&applet, &revision, expected_grant, permission, mode)
    })
  }

  fn edit_detached_permission<T>(
    &self,
    applet_id: &str,
    permission: ManagedPermission,
    edit: impl FnOnce() -> Result<T, String>,
  ) -> Result<T, String> {
    let _install = self
      .store_gate
      .try_lock()
      .map_err(|_| "Wait for the applet installation to finish before changing permissions.")?;
    let _gate = self
      .view_gate
      .lock()
      .map_err(|_| "Applet views are unavailable.")?;
    let running = self
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?;
    if running
      .view
      .as_ref()
      .and_then(|(run, _)| running.runs.get(run))
      .is_some_and(|run| run.bundle.manifest.applet_id == applet_id)
    {
      return Err(format!(
        "Close the applet before changing its {} permission.",
        permission.label()
      ));
    }
    drop(running);
    edit()
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

pub fn notify_stopped(app: &AppHandle, session_id: &str) {
  let _ = app.emit_to(
    "main",
    "backend-event",
    json!({
      "session_id":session_id,
      "event":{"name":BACKEND_STOPPED,"payload":{"message":"The applet backend stopped. Reopen the applet."}}
    }),
  );
  notify_runs_changed(app);
}

pub(crate) fn notify_runs_changed(app: &AppHandle) {
  let _ = app.emit_to("main", "apell-applet-runs-changed", ());
}

fn retire_backend(app: &AppHandle, host: &Host, session_id: &str) -> Result<(), String> {
  if host.retire_run(session_id, None)?.is_some() {
    notify_stopped(app, session_id);
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

pub async fn launch(app: AppHandle, applet_id: &str, restart: bool) -> Result<Attachment, String> {
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
    .filter(|(_, run)| run.is_stopped())
    .map(|(id, _)| id.clone())
    .collect();
  for id in stopped {
    retire_backend(&app, &host, &id)?;
  }
  let existing = host
    .running
    .lock()
    .map_err(|_| "Applet state is unavailable.")?
    .available_run(applet_id)?;
  let attach = |id: &str| {
    let attachment = host.attach_run(id);
    notify_runs_changed(&app);
    attachment
  };
  if let Some(id) = &existing {
    if !restart {
      return attach(id.as_str());
    }
  }
  let store = host.bundles.clone();
  let id = applet_id.to_owned();
  let (bundle, installed) =
    tauri::async_runtime::spawn_blocking(move || store.load_with_metadata(&id))
      .await
      .map_err(|_| "Could not load the applet package.")??;
  if let Some(id) = existing {
    // Validate the replacement before stopping anything. Another shell action
    // may already have restarted it since the Store displayed the offer.
    let already_current = host
      .running
      .lock()
      .map_err(|_| "Applet state is unavailable.")?
      .runs
      .get(&id)
      .is_some_and(|run| !run.is_stopped() && run.matches_install(&installed));
    if already_current {
      return attach(id.as_str());
    }
    host
      .stop_applet_locked(applet_id, |session_id| notify_stopped(&app, session_id))
      .await?;
  }
  let bundle = Arc::new(bundle);
  let grant_source = GrantSource::from(&installed);
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
  running.runs.insert(
    session_id.clone(),
    RunningApplet {
      bundle,
      sha256: installed.sha256,
      backend,
      grant_source,
    },
  );
  drop(running);
  attach(session_id.as_str())
}

#[cfg(test)]
mod tests {
  use super::*;
  use apell_core::{
    bundles::{digest, pack_directory, StoreChannel, StoreSwitchSnapshot, API_VERSION},
    llm::{LlmConfig, LlmRequest, LlmStatus},
  };
  use std::{fs, net::Ipv4Addr, time::Duration};
  use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
  };
  use ManagedPermission::{Camera, Location, Microphone};

  const APPLET: &str = "view-fixture";
  const RUN: &str = "native-run";
  const GENERATION_DEADLINE: Duration = Duration::from_secs(5);

  fn install(host: &Host, applet_id: &str, version: &str) {
    install_with_permissions(host, applet_id, version, &["sqlite", "llm"]);
  }

  fn install_with_permissions(host: &Host, applet_id: &str, version: &str, permissions: &[&str]) {
    let package = package_with_permissions(applet_id, version, permissions);
    host.bundles.install(&package, &digest(&package)).unwrap();
  }

  fn package_with_permissions(applet_id: &str, version: &str, permissions: &[&str]) -> Vec<u8> {
    let source = tempfile::tempdir().unwrap();
    let manifest = json!({
      "schema_version": 1, "api_version": API_VERSION, "applet_id": applet_id,
      "name": "View fixture", "version": version, "permissions": permissions
    });
    fs::write(source.path().join("manifest.json"), manifest.to_string()).unwrap();
    fs::write(
      source.path().join("index.html"),
      format!("<p>{version}</p>"),
    )
    .unwrap();
    pack_directory(source.path()).unwrap()
  }

  fn reviewed_grant(host: &Host, applet_id: &str, permission: ManagedPermission) -> String {
    host
      .grants
      .reported_grant_revision(host.bundles.list_report().unwrap(), applet_id, permission)
  }

  fn seed_run(host: &Host, session_id: &str, applet_id: &str) {
    let (bundle, installed) = host.bundles.load_with_metadata(applet_id).unwrap();
    let bundle = Arc::new(bundle);
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
        sha256: installed.sha256.clone(),
        backend: None,
        grant_source: GrantSource::from(&installed),
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

  #[tokio::test]
  async fn detached_runs_fill_the_limit_and_stopping_one_preserves_other_runs_and_data() {
    let (_directory, host) = fixture();
    let original = host.bundles.installed(APPLET).unwrap();
    let grant = GrantSource::from(&original);
    host.grants.remember(Microphone, APPLET, &grant).unwrap();
    host
      .config
      .dispatch(APPLET, "config.set", json!({"key":"theme","value":"dark"}))
      .unwrap();
    let session = host.session(RUN, "sqlite.open").unwrap();
    let database_handle = database(&host, RUN);
    let foreground = host.attach_run(RUN).unwrap();
    assert!(host.detached_runs().unwrap().is_empty());
    assert_eq!(
      host
        .stop_detached_run(APPLET, RUN, |_| panic!("Foreground applet stopped"))
        .await
        .unwrap_err(),
      RUN_CHANGED
    );
    assert!(!host.detach_view(RUN, "stale-view").unwrap());
    assert!(host.detach_view(RUN, &foreground.view_id).unwrap());
    assert_eq!(
      serde_json::to_value(host.detached_runs().unwrap()).unwrap(),
      json!([{
        "session_id":RUN,"applet_id":APPLET,"name":"View fixture","version":"1.0.0",
      }])
    );

    for index in (1..MAX_RUNNING_APPLETS).rev() {
      let id = format!("neighbor-{index}");
      install(&host, &id, "1.0.0");
      seed_run(&host, &id, &id);
    }
    let neighbor = host.attach_run("neighbor-1").unwrap();
    let detached = host.detached_runs().unwrap();
    assert_eq!(detached.len(), MAX_RUNNING_APPLETS - 1);
    assert_eq!(detached[0].applet_id, "neighbor-2");
    assert_eq!(detached.last().unwrap().applet_id, APPLET);
    {
      let running = host.running.lock().unwrap();
      assert_eq!(
        running.available_run("next").unwrap_err(),
        RUN_LIMIT_REACHED
      );
      assert_eq!(running.available_run(APPLET).unwrap().as_deref(), Some(RUN));
    }
    assert_eq!(
      host
        .stop_detached_run("neighbor-1", RUN, |_| panic!("Mismatched applet stopped"))
        .await
        .unwrap_err(),
      RUN_CHANGED
    );
    host
      .stop_detached_run(APPLET, RUN, |id| assert_eq!(id, RUN))
      .await
      .unwrap();
    assert!(*session.cancellation.borrow());
    assert!(host.session(RUN, "sqlite.open").is_err());
    assert!(host
      .sqlite
      .dispatch(
        RUN,
        APPLET,
        "sqlite.query",
        json!({
          "database_handle":database_handle,"sql":"SELECT 1",
        })
      )
      .is_err());
    assert_eq!(
      host.bundles.installed(APPLET).unwrap().sha256,
      original.sha256
    );
    assert!(host.grants.remembered(Microphone, APPLET, &grant).unwrap());
    assert_eq!(
      host
        .config
        .dispatch(APPLET, "config.get", json!({"key":"theme"}))
        .unwrap(),
      json!({"found":true,"value":"dark"})
    );
    host.require_view("neighbor-1", &neighbor.view_id).unwrap();
    assert!(host
      .running
      .lock()
      .unwrap()
      .available_run("next")
      .unwrap()
      .is_none());

    seed_run(&host, "replacement", APPLET);
    assert_eq!(
      host
        .stop_detached_run(APPLET, RUN, |_| panic!("Replacement applet stopped"))
        .await
        .unwrap_err(),
      RUN_CHANGED
    );
    assert!(host
      .detached_runs()
      .unwrap()
      .iter()
      .any(|run| run.session_id == "replacement"));
  }

  #[tokio::test]
  async fn removal_retires_a_background_run_and_resets_only_its_grants() {
    let (_directory, host) = fixture();
    let old_view = host.attach_run(RUN).unwrap();
    let run = host.session(RUN, "sqlite.open").unwrap();
    let source = host.view_grant_source(&old_view.view_id).unwrap();
    host.detach_view(RUN, &old_view.view_id).unwrap();
    host.grants.remember(Microphone, APPLET, &source).unwrap();
    host
      .config
      .dispatch(APPLET, "config.set", json!({"key":"theme","value":"dark"}))
      .unwrap();

    install(&host, "neighbor", "1.0.0");
    seed_run(&host, "neighbor-run", "neighbor");
    let current = host.attach_run("neighbor-run").unwrap();
    let neighbor = host.view_grant_source(&current.view_id).unwrap();
    host.grants.remember(Camera, "neighbor", &neighbor).unwrap();

    let mut stopped = None;
    host
      .remove_applet(APPLET, |id| stopped = Some(id.to_owned()))
      .await
      .unwrap();
    assert_eq!(stopped.as_deref(), Some(RUN));
    assert!(*run.cancellation.borrow());
    assert!(host.session(RUN, "sqlite.open").is_err());
    assert!(host.attach_run(RUN).is_err());
    assert!(host.bundles.installed(APPLET).is_err());
    assert!(!host.grants.remembered(Microphone, APPLET, &source).unwrap());
    assert!(host
      .grants
      .remembered(Camera, "neighbor", &neighbor)
      .unwrap());
    host.require_view("neighbor-run", &current.view_id).unwrap();

    install(&host, APPLET, "1.0.0");
    assert_eq!(
      host
        .config
        .dispatch(APPLET, "config.get", json!({"key":"theme"}))
        .unwrap(),
      json!({"found":true,"value":"dark"})
    );
  }

  #[test]
  fn closing_view_and_forgetting_permission_prevents_late_grant() {
    let (_directory, host) = fixture();
    let attachment = host.attach_run(RUN).unwrap();
    let source = host.view_grant_source(&attachment.view_id).unwrap();
    let session = host.sessions.lock().unwrap()[&attachment.view_id].clone();
    host.grants.remember(Microphone, APPLET, &source).unwrap();

    let remembered = reviewed_grant(&host, APPLET, Microphone);
    assert!(host
      .forget_permission(APPLET, Microphone, &remembered)
      .is_err());
    host.detach_view(RUN, &attachment.view_id).unwrap();
    host
      .forget_permission(APPLET, Microphone, &remembered)
      .unwrap();
    assert!(session
      .run_local(|| host.grants.remember(Microphone, APPLET, &source))
      .is_err());
    assert!(!host.grants.remembered(Microphone, APPLET, &source).unwrap());
    assert!(host.running.lock().unwrap().runs.contains_key(RUN));
  }

  #[test]
  fn settings_changes_apply_only_to_the_reviewed_detached_installation() {
    let (_directory, host) = fixture();
    let id = "permission-fixture";
    install_with_permissions(&host, id, "1.0.0", &["audio", "camera"]);
    let installed = host.bundles.installed(id).unwrap();
    let revision = installed_revision(&installed).unwrap();
    let source = GrantSource::from(&installed);
    let camera = reviewed_grant(&host, id, Camera);
    let microphone = reviewed_grant(&host, id, Microphone);

    assert!(host
      .set_installed_permission(id, &revision, &camera, Camera, PermissionMode::Remembered)
      .is_err());
    assert_eq!(
      host
        .set_installed_permission(
          id,
          &revision,
          &microphone,
          Microphone,
          PermissionMode::Blocked
        )
        .unwrap(),
      PermissionMode::Blocked
    );
    assert_eq!(
      host
        .set_installed_permission(
          id,
          &revision,
          &microphone,
          Microphone,
          PermissionMode::Remembered
        )
        .unwrap_err(),
      GRANT_CHANGED
    );
    assert_eq!(
      host.grants.mode(Microphone, id, &source).unwrap(),
      PermissionMode::Blocked
    );

    let blocked = reviewed_grant(&host, id, Microphone);
    seed_run(&host, "permission-fixture-run", id);
    let current = host.attach_run("permission-fixture-run").unwrap();
    assert!(host
      .set_installed_permission(
        id,
        &revision,
        &blocked,
        Microphone,
        PermissionMode::Remembered
      )
      .is_err());
    host
      .detach_view("permission-fixture-run", &current.view_id)
      .unwrap();

    install_with_permissions(&host, id, "2.0.0", &["audio", "camera", "fs"]);
    assert_eq!(
      host
        .set_installed_permission(
          id,
          &revision,
          &blocked,
          Microphone,
          PermissionMode::Remembered
        )
        .unwrap_err(),
      GRANT_CHANGED
    );
    assert_eq!(
      host
        .forget_permission(id, Microphone, &blocked)
        .unwrap_err(),
      GRANT_CHANGED
    );
    let updated = host.bundles.installed(id).unwrap();
    let new_revision = installed_revision(&updated).unwrap();
    assert_eq!(
      host
        .grants
        .mode(Microphone, id, &GrantSource::from(&updated))
        .unwrap(),
      PermissionMode::Ask
    );
    let new_source = GrantSource::from(&updated);
    let new_microphone = reviewed_grant(&host, id, Microphone);
    host
      .set_installed_permission(
        id,
        &new_revision,
        &new_microphone,
        Microphone,
        PermissionMode::Blocked,
      )
      .unwrap();
    let old_view = host.attach_run("permission-fixture-run").unwrap();
    assert!(host
      .view_permission(
        "permission-fixture-run",
        &old_view.view_id,
        Microphone,
        Some(PermissionMode::Remembered)
      )
      .is_err());
    host
      .remember_installed_permission(Microphone, id, &source)
      .unwrap();
    assert_eq!(
      host.grants.mode(Microphone, id, &new_source).unwrap(),
      PermissionMode::Blocked
    );
    host
      .detach_view("permission-fixture-run", &old_view.view_id)
      .unwrap();
    let new_camera = reviewed_grant(&host, id, Camera);
    assert_eq!(
      host
        .set_installed_permission(
          id,
          &new_revision,
          &new_camera,
          Camera,
          PermissionMode::Remembered
        )
        .unwrap(),
      PermissionMode::Remembered
    );
  }

  #[test]
  fn settings_reset_binds_the_reviewed_store_source_even_with_identical_bytes() {
    let (_directory, host) = fixture();
    let applet = host.bundles.installed(APPLET).unwrap();
    let bytes = fs::read(
      host
        .root
        .join("bundles")
        .join(APPLET)
        .join(format!("{}.apel", applet.sha256)),
    )
    .unwrap();
    let source = GrantSource::from(&applet);
    host.grants.remember(Microphone, APPLET, &source).unwrap();
    let reviewed = reviewed_grant(&host, APPLET, Microphone);

    host
      .bundles
      .switch_to_store_release(
        &bytes,
        &applet.sha256,
        StoreSource {
          store_url: "https://store.example".into(),
          applet_id: APPLET.into(),
          channel: StoreChannel::Stable,
        },
        &StoreSwitchSnapshot {
          sha256: applet.sha256.clone(),
          source: applet.source,
          removed: false,
        },
      )
      .unwrap();

    assert_eq!(
      host
        .forget_permission(APPLET, Microphone, &reviewed)
        .unwrap_err(),
      GRANT_CHANGED
    );
    assert!(host.grants.remembered(Microphone, APPLET, &source).unwrap());
    let moved = reviewed_grant(&host, APPLET, Microphone);
    host.forget_permission(APPLET, Microphone, &moved).unwrap();
    assert!(!host.grants.remembered(Microphone, APPLET, &source).unwrap());
  }

  #[test]
  fn settings_reset_allows_an_unavailable_grant_only_for_that_review() {
    let (_directory, host) = fixture();
    let applet = host.bundles.installed(APPLET).unwrap();
    let archive = host
      .root
      .join("bundles")
      .join(APPLET)
      .join(format!("{}.apel", applet.sha256));
    let bytes = fs::read(&archive).unwrap();
    let source = GrantSource::from(&applet);
    host.grants.remember(Microphone, APPLET, &source).unwrap();
    let reviewed = reviewed_grant(&host, APPLET, Microphone);

    fs::remove_file(archive).unwrap();
    let unavailable = reviewed_grant(&host, APPLET, Microphone);
    assert_eq!(
      host
        .forget_permission(APPLET, Microphone, &reviewed)
        .unwrap_err(),
      GRANT_CHANGED
    );
    host
      .forget_permission(APPLET, Microphone, &unavailable)
      .unwrap();
    assert!(!host.grants.remembered(Microphone, APPLET, &source).unwrap());
    assert_eq!(
      host
        .forget_permission(APPLET, Microphone, &unavailable)
        .unwrap_err(),
      GRANT_CHANGED
    );

    host.grants.remember(Microphone, APPLET, &source).unwrap();
    let report = host.bundles.list_report().unwrap();
    let snapshot = report.unavailable[0].restore_snapshot.as_ref().unwrap();
    host
      .bundles
      .restore_package(APPLET, snapshot, &bytes)
      .unwrap();
    assert_eq!(
      host
        .forget_permission(APPLET, Microphone, &unavailable)
        .unwrap_err(),
      GRANT_CHANGED
    );
    assert!(host.grants.remembered(Microphone, APPLET, &source).unwrap());

    host.bundles.remove(APPLET, || Ok(())).unwrap();
    assert_eq!(
      host
        .forget_permission(APPLET, Microphone, &reviewed)
        .unwrap_err(),
      GRANT_CHANGED
    );
  }

  #[test]
  fn about_permission_uses_the_attached_package_and_rejects_stale_views() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::new(dir.path().into()).unwrap();
    install_with_permissions(&host, APPLET, "1.0.0", &["camera", "fs"]);
    seed_run(&host, RUN, APPLET);
    let first = host.attach_run(RUN).unwrap();
    let first_source = host.view_grant_source(&first.view_id).unwrap();
    assert_eq!(
      host
        .view_permission(RUN, &first.view_id, Camera, None)
        .unwrap(),
      ViewPermission {
        mode: PermissionMode::Ask,
        edit_blocker: None,
      }
    );
    assert!(host
      .view_permission("different", &first.view_id, Camera, None)
      .is_err());
    host
      .view_permission(
        RUN,
        &first.view_id,
        Camera,
        Some(PermissionMode::Remembered),
      )
      .unwrap();

    let reloaded = host.attach_view(RUN, &first.view_id).unwrap();
    assert!(host
      .view_permission(RUN, &first.view_id, Camera, Some(PermissionMode::Ask))
      .is_err());
    assert_eq!(
      host
        .view_permission(RUN, &reloaded.view_id, Camera, None)
        .unwrap(),
      ViewPermission {
        mode: PermissionMode::Remembered,
        edit_blocker: None,
      }
    );

    install_with_permissions(&host, APPLET, "2.0.0", &["camera", "fs"]);
    assert_eq!(
      host
        .view_permission(RUN, &reloaded.view_id, Camera, None)
        .unwrap(),
      ViewPermission {
        mode: PermissionMode::Remembered,
        edit_blocker: Some(PermissionEditBlocker::SourceChanged),
      }
    );
    host.retire_run(RUN, None).unwrap();
    seed_run(&host, "reopened", APPLET);
    let replaced = host.attach_run("reopened").unwrap();
    assert_eq!(
      host
        .view_permission(
          "reopened",
          &replaced.view_id,
          Camera,
          Some(PermissionMode::Ask)
        )
        .unwrap()
        .mode,
      PermissionMode::Ask
    );
    assert!(host
      .grants
      .remembered(Camera, APPLET, &first_source)
      .unwrap());
    host
      .view_permission(
        "reopened",
        &replaced.view_id,
        Camera,
        Some(PermissionMode::Remembered),
      )
      .unwrap();
    assert!(!host
      .grants
      .remembered(Camera, APPLET, &first_source)
      .unwrap());
    host
      .view_permission(
        "reopened",
        &replaced.view_id,
        Camera,
        Some(PermissionMode::Ask),
      )
      .unwrap();
    assert_eq!(
      host
        .view_permission("reopened", &replaced.view_id, Camera, None)
        .unwrap()
        .mode,
      PermissionMode::Ask
    );
  }

  #[test]
  fn about_permission_checks_the_installed_store_capability_and_availability() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::new(root.path().to_owned()).unwrap();
    let source = StoreSource {
      store_url: "https://store.example.invalid".into(),
      applet_id: APPLET.into(),
      channel: apell_core::bundles::StoreChannel::Stable,
    };
    let install = |version, permissions: &[&str], expected| {
      let package = package_with_permissions(APPLET, version, permissions);
      host
        .bundles
        .install_store_release(&package, &digest(&package), source.clone(), expected)
        .unwrap()
    };
    let first = install("1.0.0", &["camera", "fs"], None);
    seed_run(&host, RUN, APPLET);
    let attached = host.attach_run(RUN).unwrap();
    let view = &attached.view_id;

    let second = install("2.0.0", &["camera", "fs"], Some(first.sha256.as_str()));
    assert!(host.needs_applet_restart(&second));
    assert_eq!(
      host
        .view_permission(RUN, view, Camera, Some(PermissionMode::Blocked))
        .unwrap(),
      ViewPermission {
        mode: PermissionMode::Blocked,
        edit_blocker: None,
      }
    );

    let third = install("3.0.0", &["camera"], Some(second.sha256.as_str()));
    assert_eq!(
      host.view_permission(RUN, view, Camera, None).unwrap(),
      ViewPermission {
        mode: PermissionMode::Blocked,
        edit_blocker: Some(PermissionEditBlocker::PermissionRemoved),
      }
    );
    assert_eq!(
      host
        .view_permission(RUN, view, Camera, Some(PermissionMode::Remembered))
        .unwrap_err(),
      PermissionEditBlocker::PermissionRemoved.message()
    );
    fs::write(
      root
        .path()
        .join("bundles")
        .join(APPLET)
        .join(format!("{}.apel", third.sha256)),
      b"damaged fixture package",
    )
    .unwrap();
    assert_eq!(
      host.view_permission(RUN, view, Camera, None).unwrap(),
      ViewPermission {
        mode: PermissionMode::Blocked,
        edit_blocker: Some(PermissionEditBlocker::InstallationUnavailable),
      }
    );
    assert_eq!(
      host
        .view_permission(RUN, view, Camera, Some(PermissionMode::Remembered))
        .unwrap_err(),
      PermissionEditBlocker::InstallationUnavailable.message()
    );
    assert_eq!(
      host
        .grants
        .mode(Camera, APPLET, &GrantSource::from(&third))
        .unwrap(),
      PermissionMode::Blocked
    );
  }

  #[test]
  fn about_permission_requires_its_declared_manifest_group() {
    let (_directory, host) = fixture();
    let attachment = host.attach_run(RUN).unwrap();
    for (permission, group) in [
      (Microphone, "audio"),
      (Camera, "camera"),
      (Location, "location"),
    ] {
      for update in [None, Some(PermissionMode::Remembered)] {
        assert_eq!(
          host
            .view_permission(RUN, &attachment.view_id, permission, update)
            .unwrap_err(),
          format!("This applet did not declare {group} access.")
        );
      }
    }
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

  async fn generate(host: &Host, session: &str) -> (JoinHandle<LlmStatus>, oneshot::Sender<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let config = LlmConfig {
      provider_id: "custom".into(),
      base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
      model_id: "fixture-model".into(),
      connection_id: "fixture-connection".into(),
    };
    let (received, mut ready) = oneshot::channel();
    let (complete, release) = oneshot::channel();
    tokio::spawn(async move {
      let (socket, _) = listener.accept().await.unwrap();
      let mut stream = BufReader::new(socket);
      let mut content_length = None;
      loop {
        let mut line = String::new();
        assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
        if let Some((name, value)) = line.split_once(':') {
          if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(value.trim().parse::<usize>().unwrap());
          }
        }
        if line == "\r\n" {
          break;
        }
      }
      let mut body = vec![0; content_length.expect("Generation sends a JSON request body.")];
      stream.read_exact(&mut body).await.unwrap();
      received.send(()).unwrap();
      if release.await.is_ok() {
        let body = format!(
          "data: {}\n\n",
          json!({"model": "fixture-model", "choices": [
            {"index": 0, "delta": {"content": "Hello"}, "finish_reason": "stop"}
          ]})
        );
        let response = format!(
          "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
      }
    });
    let llm = host.llm.clone();
    let session = session.to_owned();
    let request = LlmRequest {
      request_id: "generation".into(),
      messages: vec![json!({"role": "user", "content": "Hello"})],
      ..Default::default()
    };
    let mut work = tokio::spawn(async move {
      llm
        .generate(&session, request, config, None, |_| {})
        .await
        .status
    });
    tokio::select! {
      result = &mut work => panic!("Generation ended before view rotation: {result:?}"),
      result = &mut ready => result.unwrap(),
      _ = tokio::time::sleep(GENERATION_DEADLINE) => panic!("Generation did not reach the provider"),
    }
    (work, complete)
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
    let (run_generation, finish_run) = generate(&host, RUN).await;
    let (view_generation, finish_view) = generate(&host, &first.view_id).await;

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
    assert_eq!(
      tokio::time::timeout(GENERATION_DEADLINE, view_generation)
        .await
        .unwrap()
        .unwrap(),
      LlmStatus::Cancelled
    );
    drop(finish_view);

    assert!(!*run_session.cancellation.borrow());
    run_session.run_local(|| Ok(())).unwrap();
    assert!(query(RUN, &run_database).is_ok());
    assert!(query(&second.view_id, &database(&host, &second.view_id)).is_ok());
    finish_run.send(()).unwrap();
    assert_eq!(
      tokio::time::timeout(GENERATION_DEADLINE, run_generation)
        .await
        .unwrap()
        .unwrap(),
      LlmStatus::Completed
    );
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

  #[tokio::test]
  async fn restarting_an_applet_revokes_only_its_run_and_view_and_keeps_data() {
    let (_directory, host) = fixture();
    let old = host.attach_run(RUN).unwrap();
    let run_session = host.session(RUN, "llm.generate").unwrap();
    let view_session = host.session(&old.view_id, "llm.generate").unwrap();
    let database_handle = database(&host, RUN);
    host
      .config
      .dispatch(APPLET, "config.set", json!({"key":"theme","value":"dark"}))
      .unwrap();
    assert!(!host.needs_applet_restart(&host.bundles.installed(APPLET).unwrap()));
    install(&host, "neighbor", "1.0.0");
    seed_run(&host, "neighbor-run", "neighbor");
    install(&host, "neighbor", "2.0.0");
    install(&host, APPLET, "2.0.0");
    assert!(host.needs_applet_restart(&host.bundles.installed(APPLET).unwrap()));

    let _launch = host.run_gate.lock().await;
    host
      .stop_applet_locked(APPLET, |id| assert_eq!(id, RUN))
      .await
      .unwrap();
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
    assert!(!host.needs_applet_restart(&host.bundles.installed(APPLET).unwrap()));
    assert_eq!(
      host
        .config
        .dispatch(APPLET, "config.get", json!({"key":"theme"}))
        .unwrap(),
      json!({"found":true,"value":"dark"})
    );
    assert!(host.retire_run(RUN, None).unwrap().is_none());
    assert!(host.retire_run("neighbor-run", None).unwrap().is_some());
    host.require_view("reopened", &reopened.view_id).unwrap();
    assert_eq!(host.running.lock().unwrap().runs.len(), 1);
    assert_eq!(host.sessions.lock().unwrap().len(), 2);
  }

  #[test]
  fn about_reports_the_current_installation_even_for_an_older_running_package() {
    let (_directory, host) = fixture();
    let attachment = host.attach_run(RUN).unwrap();
    let initial = host.view_installation(RUN, &attachment.view_id).unwrap();
    assert_eq!(initial.installed_version, "1.0.0");
    assert!(initial.store_source.is_none());
    assert!(!initial.restart_required);
    let installed = host.bundles.installed(APPLET).unwrap();
    assert_eq!(initial.installed_sha256, installed.sha256);
    let bytes = fs::read(
      host
        .root
        .join("bundles")
        .join(APPLET)
        .join(format!("{}.apel", installed.sha256)),
    )
    .unwrap();
    let updated = host
      .bundles
      .switch_to_store_release(
        &bytes,
        &installed.sha256,
        StoreSource {
          store_url: "https://store.example".into(),
          applet_id: APPLET.into(),
          channel: StoreChannel::Stable,
        },
        &StoreSwitchSnapshot {
          sha256: installed.sha256.clone(),
          source: installed.source,
          removed: false,
        },
      )
      .unwrap();
    assert_eq!(updated.sha256, installed.sha256);
    assert!(host.needs_applet_restart(&updated));
    let installation = host.view_installation(RUN, &attachment.view_id).unwrap();
    assert_eq!(installation.installed_version, "1.0.0");
    assert_eq!(installation.installed_sha256, updated.sha256);
    let source = installation.store_source.unwrap();
    assert_eq!(source.store_url, "https://store.example");
    assert_eq!(source.channel, StoreChannel::Stable);
    assert!(installation.restart_required);
    assert!(host
      .view_installation("wrong-run", &attachment.view_id)
      .is_err());
    host.attach_view(RUN, &attachment.view_id).unwrap();
    assert!(host.view_installation(RUN, &attachment.view_id).is_err());
  }
}

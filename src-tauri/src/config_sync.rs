mod automatic;
mod snapshot;

pub(crate) use automatic::{clear_activity, kick, start};

use crate::{
  account::Access,
  commands::blocking,
  host::Host,
  key_sync::BackgroundStop,
  provider_commands::{commit_settings, finish_snapshot, validate_saved_route},
  settings::{Connection, Settings, LEGACY_DEMO_CONNECTION_ID},
};
use apell_core::bundles::{atomic_write, digest};
use automatic::{Automatic, SyncStop};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use snapshot::{CloudApplyBlocker, Snapshot};
use std::{collections::HashSet, io::Read, path::PathBuf, time::Duration};
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

const HEAD_PATH: &str = "/api/v1/config-sync/head";
const MAX_REPLY_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub struct ConfigSync {
  pub(crate) gate: tokio::sync::Mutex<()>,
  toggle: tokio::sync::Mutex<()>,
  automatic: Automatic,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeviceState {
  enabled: bool,
  cursor: Option<Cursor>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pending_conflict_id: Option<String>,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Cursor {
  revision_id: String,
  snapshot_hash: String,
  remote_active: String,
  local_active: String,
  updated_at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Head {
  head: Option<Revision>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revision {
  revision_id: String,
  parent_revision_ids: Vec<String>,
  snapshot: Snapshot,
  updated_at: String,
}

struct ComparedSettings {
  head: Option<Revision>,
  local: Snapshot,
  local_active: Option<String>,
  local_hash: String,
  cloud_apply_blocker: Option<CloudApplyBlocker>,
}

struct SyncOptions<'a> {
  stop: &'a SyncStop,
  automatic: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enable {
  enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Resolution {
  KeepLocal,
  UseCloud,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
  resolution: Option<Resolution>,
  conflict_id: Option<String>,
}

pub async fn dispatch(
  app: AppHandle,
  _host: &Host,
  method: &str,
  params: Value,
) -> Result<Value, String> {
  let method = method.to_owned();
  finish_snapshot(async move {
    let host = app.state::<Host>();
    let stop = SyncStop::manual(host.key_sync.background_stop());
    let account_key = stop.wait(host.account.account_key(&app)).await??;
    match method.as_str() {
      "sync.status" => {
        let Some(key) = account_key else {
          return Ok(status(None, false));
        };
        // Cursor files are replaced atomically. Reads do not queue behind a remote request.
        let state = stop.wait(load_state(&host, &key)).await??;
        let mut result = stop.wait(local_status(&host, &state)).await??;
        if state.enabled {
          let activity = host.config_sync.automatic.activity(&key).await;
          result["syncing"] = json!(activity.syncing);
          result["sync_error"] = json!(activity.error);
        }
        require_identity(&app, &host, &key, &stop).await?;
        Ok(result)
      }
      "sync.set_enabled" => {
        let enabled = input::<Enable>(params)?.enabled;
        let key = account_key.ok_or("Sign in to Apell Account to enable sync.")?;
        set_enabled(&app, &host, &key, enabled, &stop).await
      }
      "sync.run" | "sync.review" => {
        let key = account_key.ok_or("Sign in to Apell Account to sync settings.")?;
        let _gate = stop.wait(host.config_sync.gate.lock()).await?;
        let mut state = stop.wait(load_state(&host, &key)).await??;
        if !state.enabled {
          return Err("Turn on config sync first.".into());
        }
        let request: Run = input(params)?;
        if request.resolution.is_some() != request.conflict_id.is_some() {
          return Err("Refresh the conflict before choosing which settings to use.".into());
        }
        let review_only = method == "sync.review";
        if review_only && request.resolution.is_some() {
          return Err("A read-only sync review cannot choose settings.".into());
        }
        let access = stop.access(&app, &host).await?;
        if access.account_key != key {
          return Err("The signed-in Account changed. Try syncing again.".into());
        }
        let compared = compare_settings(&app, &host, &access, &state, &stop).await?;
        if review_only {
          require_identity(&app, &host, &key, &stop).await?;
          let mut result = review_settings(&key, &state, &compared);
          let pending = result["conflict_id"].as_str().map(str::to_owned);
          if state.pending_conflict_id != pending {
            result["status"]["pending_conflict_id"] = json!(pending);
            state.pending_conflict_id = pending;
            save_state(&host, &key, state).await?;
            let _ = app.emit_to("main", "apell-llm-settings-changed", ());
          }
          return Ok(result);
        }
        let result = synchronize(
          &app,
          &host,
          &access,
          state,
          request,
          compared,
          SyncOptions {
            stop: &stop,
            automatic: false,
          },
        )
        .await?;
        clear_activity(&app, &host).await;
        Ok(result)
      }
      _ => Err("Unknown config sync operation.".into()),
    }
  })
  .await
}

async fn set_enabled(
  app: &AppHandle,
  host: &Host,
  account: &str,
  enabled: bool,
  stop: &SyncStop,
) -> Result<Value, String> {
  let _toggle = stop.wait(host.config_sync.toggle.lock()).await?;
  // Signal first so opting out stops an automatic network wait without
  // interrupting local settings or cursor writes already in progress.
  let pause = (!enabled).then(|| host.config_sync.automatic.pause());
  let result = async {
    let _gate = stop.wait(host.config_sync.gate.lock()).await?;
    let mut state = stop.wait(load_state(host, account)).await??;
    require_identity(app, host, account, stop).await?;
    let changed = state.enabled != enabled;
    state.enabled = enabled;
    let result = stop.wait(local_status(host, &state)).await??;
    stop.check()?;
    save_state(host, account, state).await?;
    if changed {
      let _ = app.emit_to("main", "apell-llm-settings-changed", ());
    }
    clear_activity(app, host).await;
    Ok(result)
  }
  .await;
  drop(pause);
  if enabled || result.is_err() {
    kick(app);
  }
  if enabled && result.is_ok() {
    crate::key_sync::kick(app);
  }
  result
}

fn input<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
  serde_json::from_value(value).map_err(|_| "Invalid config sync request.".into())
}

fn status(state: Option<&DeviceState>, local_changes: bool) -> Value {
  let Some(state) = state else {
    return json!({"state":"disabled","last_synced_at":null,"local_changes":false,
      "pending_conflict_id":null,"syncing":false,"sync_error":null});
  };
  json!({
    "state": if state.enabled { "ready" } else { "off" },
    "last_synced_at": state.cursor.as_ref().map(|cursor| &cursor.updated_at),
    "local_changes": local_changes,
    "pending_conflict_id": if state.enabled { state.pending_conflict_id.as_ref() } else { None },
    "syncing": false,
    "sync_error": null,
  })
}

fn has_local_changes(state: &DeviceState, local: &Snapshot, hash: &str) -> bool {
  state.enabled
    && state
      .cursor
      .as_ref()
      .map_or(!local.is_empty(), |cursor| cursor.snapshot_hash != hash)
}

async fn local_status(host: &Host, state: &DeviceState) -> Result<Value, String> {
  if !state.enabled {
    return Ok(status(Some(state), false));
  }
  let local = capture(&*host.settings.lock().await, state);
  Ok(status(
    Some(state),
    has_local_changes(state, &local, &local.fingerprint()?),
  ))
}

fn device_path(host: &Host, key: &str) -> Result<PathBuf, String> {
  if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    return Err("The Account identity is invalid.".into());
  }
  Ok(host.root.join("config-sync").join(format!("{key}.json")))
}

async fn load_state(host: &Host, key: &str) -> Result<DeviceState, String> {
  let path = device_path(host, key)?;
  blocking(move || {
    let file = match std::fs::File::open(path) {
      Ok(file) => file,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
        return Ok(DeviceState::default())
      }
      Err(_) => return Err("Could not read config sync settings.".into()),
    };
    let mut bytes = Vec::new();
    file
      .take(4097)
      .read_to_end(&mut bytes)
      .map_err(|_| "Could not read config sync settings.")?;
    if bytes.len() > 4096 {
      return Err("Config sync settings exceed their storage limit.".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Config sync settings are invalid.".into())
  })
  .await
}

async fn save_state(host: &Host, key: &str, state: DeviceState) -> Result<(), String> {
  let path = device_path(host, key)?;
  blocking(move || {
    let bytes = serde_json::to_vec(&state).map_err(|_| "Could not encode config sync settings.")?;
    atomic_write(&path, &bytes)
  })
  .await
}

fn capture(settings: &Settings, state: &DeviceState) -> Snapshot {
  let preferred = state.cursor.as_ref().and_then(|cursor| {
    (settings.active_connection_id.as_deref() == saved_selection(&cursor.local_active)
      || settings.active().is_some_and(snapshot::device_only))
    .then_some(cursor.remote_active.as_str())
  });
  Snapshot::capture(settings, preferred)
}

fn saved_selection(id: &str) -> Option<&str> {
  (id != LEGACY_DEMO_CONNECTION_ID).then_some(id)
}

fn pending_preference<'a>(state: &'a DeviceState, settings: &Settings) -> Option<&'a str> {
  if !state.enabled
    || state.pending_conflict_id.is_some()
    || settings.active().is_some_and(snapshot::device_only)
  {
    return None;
  }
  let cursor = state.cursor.as_ref()?;
  let preferred = saved_selection(&cursor.remote_active)?;
  let current = settings.active_connection_id.as_deref();
  if current == Some(preferred)
    || current != saved_selection(&cursor.local_active)
    || capture(settings, state).fingerprint().ok()? != cursor.snapshot_hash
  {
    return None;
  }
  Some(preferred)
}

pub(crate) async fn pending_preferred_connection_id(
  app: &AppHandle,
  host: &Host,
  settings: &Settings,
) -> Option<String> {
  let account = host.account.account_key(app).await.ok()??;
  let state = load_state(host, &account).await.ok()?;
  let preferred = settings
    .connection(pending_preference(&state, settings)?)
    .ok()?;
  // Loading the optional cursor may overlap with an Account change.
  let current = host.account.account_key(app).await.ok()??;
  (account == current).then(|| preferred.connection_id.clone())
}

async fn compare_settings(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  state: &DeviceState,
  stop: &SyncStop,
) -> Result<ComparedSettings, String> {
  let head = stop
    .wait(request_head(host, access, Method::GET, None))
    .await??;
  require_identity(app, host, &access.account_key, stop).await?;
  let (mut local, local_active, cloud_apply_blocker) = {
    let settings = stop.wait(host.settings.lock()).await?;
    (
      capture(&settings, state),
      settings.active_connection_id.clone(),
      head
        .as_ref()
        .and_then(|head| head.snapshot.cloud_apply_blocker(&settings)),
    )
  };
  local.validate()?;
  let local_hash = local.fingerprint()?;
  Ok(ComparedSettings {
    head,
    local,
    local_active,
    local_hash,
    cloud_apply_blocker,
  })
}

fn review_settings(account_key: &str, state: &DeviceState, compared: &ComparedSettings) -> Value {
  let local_changes = has_local_changes(state, &compared.local, &compared.local_hash);
  match &compared.head {
    Some(remote) if compared.local != remote.snapshot => {
      let conflict = conflict_id(account_key, remote, &compared.local_hash);
      conflict_result(
        state,
        conflict,
        &compared.local,
        &remote.snapshot,
        local_changes,
        compared.cloud_apply_blocker.as_ref(),
      )
    }
    _ => json!({"state":"review_clear","status":status(Some(state), local_changes)}),
  }
}

fn conflict_result(
  state: &DeviceState,
  conflict: String,
  local: &Snapshot,
  cloud: &Snapshot,
  local_changes: bool,
  cloud_apply_blocker: Option<&CloudApplyBlocker>,
) -> Value {
  json!({
    "state":"conflict",
    "status":status(Some(state), local_changes),
    "conflict_id":conflict,
    "local":local.summary(),
    "cloud":cloud.summary(),
    "cloud_apply_blocker":cloud_apply_blocker,
  })
}

async fn synchronize(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  mut state: DeviceState,
  input: Run,
  compared: ComparedSettings,
  options: SyncOptions<'_>,
) -> Result<Value, String> {
  let stop = options.stop;
  let ComparedSettings {
    head,
    local,
    local_active,
    local_hash,
    cloud_apply_blocker,
  } = compared;
  let result = match head {
    None if input.resolution.is_some() => {
      return Err("The model settings changed. Refresh the review.".into())
    }
    None if !first_upload_required(&state, &local)? => {
      require_identity(app, host, &access.account_key, stop).await?;
      return Ok(json!({"state":"current", "status":status(Some(&state), false)}));
    }
    None => (
      "uploaded",
      stop.wait(upload(host, access, &local, None)).await??,
      local_active,
      false,
    ),
    Some(remote) => {
      let conflict = conflict_id(&access.account_key, &remote, &local_hash);
      if input.conflict_id.as_ref().is_some_and(|id| *id != conflict) {
        return Err("The model settings changed. Refresh the review.".into());
      }
      let allow_removals = matches!(input.resolution, Some(Resolution::UseCloud));
      let action = decide_sync(
        &local,
        &local_hash,
        &remote,
        &state,
        input.resolution,
        cloud_apply_blocker.as_ref(),
      );
      match action {
        Action::Current => {
          let activated =
            activate_pending_preference(app, host, access, &state, &local_hash, stop).await?;
          let applied = activated.is_some();
          ("current", remote, activated.or(local_active), applied)
        }
        Action::Upload => {
          let revision = stop
            .wait(upload(host, access, &local, Some(remote.revision_id)))
            .await??;
          ("uploaded", revision, local_active, false)
        }
        Action::Download => {
          let active = apply_remote(
            app,
            host,
            access,
            &state,
            &local_hash,
            &remote.snapshot,
            options,
            allow_removals,
          )
          .await?;
          ("downloaded", remote, active, true)
        }
        Action::Conflict => {
          require_identity(app, host, &access.account_key, stop).await?;
          let changed = state.pending_conflict_id.as_deref() != Some(conflict.as_str());
          state.pending_conflict_id = Some(conflict.clone());
          let output = conflict_result(
            &state,
            conflict,
            &local,
            &remote.snapshot,
            has_local_changes(&state, &local, &local_hash),
            cloud_apply_blocker.as_ref(),
          );
          if changed {
            save_state(host, &access.account_key, state).await?;
            let _ = app.emit_to("main", "apell-llm-settings-changed", ());
          }
          return Ok(output);
        }
      }
    }
  };
  let (outcome, revision, local_active, applied) = result;
  // A completed local apply includes its cursor write even if sign-out began
  // during credential storage. Other outcomes can still stop before a commit.
  if applied {
    require_account(app, host, access).await?;
  } else {
    require_identity(app, host, &access.account_key, stop).await?;
  }
  let cursor = Cursor {
    snapshot_hash: revision.snapshot.fingerprint()?,
    revision_id: revision.revision_id,
    remote_active: revision.snapshot.active_connection_id,
    local_active: local_active.unwrap_or_else(|| LEGACY_DEMO_CONNECTION_ID.to_owned()),
    updated_at: revision.updated_at,
  };
  let changed = state.cursor.as_ref() != Some(&cursor) || state.pending_conflict_id.is_some();
  state.cursor = Some(cursor);
  state.pending_conflict_id = None;
  let output = json!({"state": outcome, "status": local_status(host, &state).await?});
  let checkpoint = if changed {
    save_state(host, &access.account_key, state).await
  } else {
    Ok(())
  };
  if applied || (changed && checkpoint.is_ok()) {
    let _ = app.emit_to("main", "apell-llm-settings-changed", ());
  }
  checkpoint?;
  if changed || outcome != "current" {
    crate::key_sync::kick(app);
  }
  Ok(output)
}

async fn activate_pending_preference(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  state: &DeviceState,
  local_hash: &str,
  stop: &SyncStop,
) -> Result<Option<String>, String> {
  let Some(cursor) = state.cursor.as_ref() else {
    return Ok(None);
  };
  if cursor.local_active == cursor.remote_active || cursor.snapshot_hash != local_hash {
    return Ok(None);
  }

  // The caller holds config_sync.gate and has compared with the current cloud
  // snapshot. Recheck local metadata and manual selection before applying it.
  let _gate = stop.wait(host.credential_gate.lock()).await?;
  let mut current = stop.wait(host.settings.lock()).await?;
  let Some(preferred) = pending_preference(state, &current) else {
    return Ok(None);
  };
  let preferred = preferred.to_owned();
  host.account.require_current_access(access).await?;
  stop.check()?;
  let work_app = app.clone();
  let root = host.root.clone();
  let next = current.clone();
  let updated = blocking(move || {
    persist_preference_if_ready(&root, next, preferred, |connection| {
      validate_saved_route(&work_app, connection).is_ok()
    })
  })
  .await?;
  let Some(next) = updated else {
    return Ok(None);
  };
  let active = next.active_connection_id.clone();
  *current = next;
  Ok(active)
}

fn persist_preference_if_ready(
  root: &std::path::Path,
  mut settings: Settings,
  preferred: String,
  ready: impl FnOnce(&Connection) -> bool,
) -> Result<Option<Settings>, String> {
  if !settings.connection(&preferred).is_ok_and(ready) {
    return Ok(None);
  }
  settings.active_connection_id = Some(preferred);
  settings.persist(root)?;
  Ok(Some(settings))
}

enum Action {
  Current,
  Upload,
  Download,
  Conflict,
}

fn first_upload_required(state: &DeviceState, local: &Snapshot) -> Result<bool, String> {
  if state.cursor.is_some() {
    return Err("The saved cloud revision is missing. Local settings were kept.".into());
  }
  if state.pending_conflict_id.is_some() {
    return Err("The reviewed cloud revision is missing. Refresh the review.".into());
  }
  Ok(!local.is_empty())
}

fn decide_sync(
  local: &Snapshot,
  hash: &str,
  remote: &Revision,
  state: &DeviceState,
  resolution: Option<Resolution>,
  cloud_apply_blocker: Option<&CloudApplyBlocker>,
) -> Action {
  let unresolved = resolution.is_none();
  if unresolved
    && *local != remote.snapshot
    && (state.pending_conflict_id.is_some()
      || matches!(
        cloud_apply_blocker,
        Some(CloudApplyBlocker::DestinationConflict { .. })
      ))
  {
    return Action::Conflict;
  }
  match decide(local, hash, remote, state.cursor.as_ref(), resolution) {
    Action::Download if unresolved && cloud_apply_blocker.is_some() => Action::Conflict,
    action => action,
  }
}

fn decide(
  local: &Snapshot,
  hash: &str,
  remote: &Revision,
  cursor: Option<&Cursor>,
  resolution: Option<Resolution>,
) -> Action {
  if *local == remote.snapshot {
    return Action::Current;
  }
  match resolution {
    Some(Resolution::KeepLocal) => Action::Upload,
    Some(Resolution::UseCloud) => Action::Download,
    None if cursor.is_some_and(|cursor| cursor.revision_id == remote.revision_id) => Action::Upload,
    None if cursor.is_none() && local.is_empty() => Action::Download,
    None
      if cursor.is_some_and(|cursor| cursor.snapshot_hash == hash)
        && !remote.snapshot.removes_connections_from(local) =>
    {
      Action::Download
    }
    None => Action::Conflict,
  }
}

fn conflict_id(account_key: &str, remote: &Revision, local_hash: &str) -> String {
  digest(format!("{account_key}:{}:{local_hash}", remote.revision_id).as_bytes())
}

async fn require_account(app: &AppHandle, host: &Host, access: &Access) -> Result<(), String> {
  if host.account.account_key(app).await?.as_deref() != Some(access.account_key.as_str()) {
    return Err("The signed-in Account changed. Try syncing again.".into());
  }
  Ok(())
}

async fn require_identity(
  app: &AppHandle,
  host: &Host,
  key: &str,
  stop: &SyncStop,
) -> Result<(), String> {
  if stop.wait(host.account.account_key(app)).await??.as_deref() != Some(key) {
    return Err("The signed-in Account changed. Try syncing again.".into());
  }
  stop.check()
}

async fn apply_remote(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  state: &DeviceState,
  local_hash: &str,
  remote: &Snapshot,
  options: SyncOptions<'_>,
  allow_removals: bool,
) -> Result<Option<String>, String> {
  let SyncOptions { stop, automatic } = options;
  let _gate = stop.wait(host.credential_gate.lock()).await?;
  let mut current = stop.wait(host.settings.lock()).await?;
  require_identity(app, host, &access.account_key, stop).await?;
  if capture(&current, state).fingerprint()? != local_hash {
    return Err("Provider settings changed during sync. Run sync again.".into());
  }
  let (mut next, removals) = remote.settings(&current)?;
  if !allow_removals && !removals.is_empty() {
    return Err("Review the model settings before removing a local connection.".into());
  }
  let app = app.clone();
  let root = host.root.clone();
  let previous_active = current.active_connection_id.clone();
  let local_only = current.active().is_some_and(snapshot::device_only);
  stop.check()?;
  let next = blocking(move || {
    if !local_only {
      retain_usable_active(&mut next, previous_active, automatic, |connection| {
        validate_saved_route(&app, connection).is_ok()
      });
    }
    commit_settings(
      &app,
      &root,
      &next,
      removals.into_iter().map(|slot| (slot, None)).collect(),
    )?;
    Ok(next)
  })
  .await?;
  let active = next.active_connection_id.clone();
  *current = next;
  Ok(active)
}

fn retain_usable_active(
  next: &mut Settings,
  previous_active: Option<String>,
  preserve_unspecified: bool,
  usable: impl Fn(&Connection) -> bool,
) {
  match next.active() {
    Some(selected) if usable(selected) => return,
    None if !preserve_unspecified => return,
    _ => {}
  }
  let previous = previous_active
    .as_deref()
    .and_then(|id| next.connection(id).ok());
  next.active_connection_id = match previous {
    Some(connection) if usable(connection) => previous_active,
    _ => None,
  };
}

pub(crate) async fn add_credential_connections(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  stop: &BackgroundStop,
  mut ids: HashSet<String>,
  retain_restorable: impl FnOnce(&mut HashSet<String>) -> Result<(), String> + Send + 'static,
) -> Result<bool, String> {
  if ids.is_empty() {
    return Ok(false);
  }
  let initial = stop.wait(load_state(host, &access.account_key)).await??;
  if !initial.enabled || initial.pending_conflict_id.is_some() {
    return Ok(false);
  }
  let Some(head) = stop
    .wait(request_head(host, access, Method::GET, None))
    .await??
  else {
    return Ok(false);
  };
  let _sync_gate = stop.wait(host.config_sync.gate.lock()).await?;
  let current_sync = stop.wait(load_state(host, &access.account_key)).await??;
  if !current_sync.enabled
    || current_sync.pending_conflict_id.is_some()
    || current_sync.cursor != initial.cursor
  {
    return Ok(false);
  }
  let _credential_gate = stop.wait(host.credential_gate.lock()).await?;
  let mut current = stop.wait(host.settings.lock()).await?;
  require_account(app, host, access).await?;
  stop.check()?;
  let root = host.root.clone();
  let mut next = current.clone();
  let updated = blocking(move || {
    // The device could have removed a connection while the head was loading.
    retain_restorable(&mut ids)?;
    let missing = head.snapshot.missing_key_connections(&next, &ids)?;
    if missing.is_empty() {
      return Ok(None);
    }
    next.connections.extend(missing);
    next.persist(&root)?;
    Ok(Some(next))
  })
  .await?;
  let Some(next) = updated else {
    return Ok(false);
  };
  *current = next;
  Ok(true)
}

pub(crate) async fn apply_key_restores<T: Send + 'static>(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  stop: &BackgroundStop,
  apply: impl FnOnce(&mut Settings, Option<&str>) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
  // Serialize the preference lookup with model sync, and the commit with local
  // connection edits. Other credential restores still work if config state is unreadable.
  let _sync_gate = stop.wait(host.config_sync.gate.lock()).await?;
  let state = load_state(host, &access.account_key).await.ok();
  let _credential_gate = stop.wait(host.credential_gate.lock()).await?;
  let mut current = stop.wait(host.settings.lock()).await?;
  require_account(app, host, access).await?;
  stop.check()?;
  let mut next = current.clone();
  let (next, result) = blocking(move || {
    let preferred = state
      .as_ref()
      .and_then(|state| pending_preference(state, &next))
      .map(str::to_owned);
    let result = apply(&mut next, preferred.as_deref());
    Ok((next, result))
  })
  .await?;
  // The restore can commit settings before a later sync checkpoint fails.
  *current = next;
  result
}

async fn upload(
  host: &Host,
  access: &Access,
  snapshot: &Snapshot,
  expected: Option<String>,
) -> Result<Revision, String> {
  let revision_id = Uuid::new_v4().to_string();
  let parent_revision_ids: Vec<_> = expected.iter().cloned().collect();
  let body = json!({
    "expected_head": expected,
    "revision_id": revision_id,
    "parent_revision_ids": parent_revision_ids,
    "snapshot": snapshot,
  });
  request_head(host, access, Method::PUT, Some(body))
    .await?
    .filter(|revision| {
      revision.revision_id == revision_id
        && revision.parent_revision_ids == parent_revision_ids
        && &revision.snapshot == snapshot
    })
    .ok_or_else(|| "The server did not confirm the uploaded settings. Run sync again.".into())
}

async fn request_head(
  host: &Host,
  access: &Access,
  method: Method,
  body: Option<Value>,
) -> Result<Option<Revision>, String> {
  let url = access
    .sync_url
    .join(HEAD_PATH)
    .map_err(|_| "Invalid config sync service address.")?;
  let mut request = host
    .account
    .http_client()
    .request(method, url)
    .bearer_auth(access.token.as_str())
    .timeout(Duration::from_secs(20));
  if let Some(body) = body {
    request = request.json(&body);
  }
  let mut response = request
    .send()
    .await
    .map_err(|_| "Could not reach config sync. Try again.")?;
  match response.status() {
    StatusCode::OK => (),
    StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => {
      return Err("Another device synced first. Run sync again to review the changes.".into())
    }
    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
      return Err("Config sync could not authorize this Account. Sign in again and retry.".into())
    }
    _ => return Err("Config sync is unavailable. Try again later.".into()),
  }
  let mut bytes = Vec::new();
  while let Some(chunk) = response
    .chunk()
    .await
    .map_err(|_| "Could not read synced settings. Try again.")?
  {
    if bytes.len() + chunk.len() > MAX_REPLY_BYTES {
      return Err("Synced settings exceed their size limit.".into());
    }
    bytes.extend_from_slice(&chunk);
  }
  let mut reply: Head =
    serde_json::from_slice(&bytes).map_err(|_| "The sync service returned invalid settings.")?;
  if let Some(revision) = &mut reply.head {
    if Uuid::parse_str(&revision.revision_id).is_err()
      || revision.parent_revision_ids.len() > 2
      || revision
        .parent_revision_ids
        .iter()
        .any(|id| Uuid::parse_str(id).is_err())
      || revision.updated_at.len() > 40
    {
      return Err("The sync service returned an invalid revision.".into());
    }
    revision.snapshot.validate()?;
  }
  Ok(reply.head)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::settings::credential_ref;

  fn snapshot(model: &str) -> Snapshot {
    let mut settings = Settings::default();
    settings.connections.push(crate::settings::Connection {
      connection_id: "fixture".into(),
      name: "Fixture".into(),
      provider_id: "openai".into(),
      base_url: apell_core::providers::default_base_url("openai")
        .unwrap()
        .into(),
      model_id: model.into(),
      credential: credential_ref("fixture"),
    });
    Snapshot::capture(&settings, None)
  }

  #[test]
  fn restoring_a_key_can_replace_only_the_fallback_from_the_last_model_sync() {
    let (mut settings, _) = snapshot("fixture-model")
      .settings(&Settings::default())
      .unwrap();
    let mut fallback = settings.connections[0].clone();
    fallback.connection_id = "fallback".into();
    fallback.credential = credential_ref("fallback");
    settings.connections.push(fallback);
    settings.active_connection_id = Some("fallback".into());
    let cloud = Snapshot::capture(&settings, Some("fixture"));
    let mut state = DeviceState {
      enabled: true,
      pending_conflict_id: None,
      cursor: Some(Cursor {
        revision_id: "fixture-revision".into(),
        snapshot_hash: cloud.fingerprint().unwrap(),
        remote_active: "fixture".into(),
        local_active: "fallback".into(),
        updated_at: String::new(),
      }),
    };
    assert_eq!(pending_preference(&state, &settings), Some("fixture"));
    settings.active_connection_id = None;
    assert_eq!(pending_preference(&state, &settings), None);
    settings.active_connection_id = Some("fallback".into());
    state.pending_conflict_id = Some("fixture-conflict".into());
    assert_eq!(pending_preference(&state, &settings), None);
  }

  #[test]
  fn ready_pending_preference_persists_without_changing_the_synced_snapshot() {
    let (mut settings, _) = snapshot("preferred-model")
      .settings(&Settings::default())
      .unwrap();
    let mut fallback = settings.connections[0].clone();
    fallback.connection_id = "fallback".into();
    fallback.credential = credential_ref("fallback");
    settings.connections.push(fallback);
    settings.active_connection_id = Some("fallback".into());
    let cloud = Snapshot::capture(&settings, Some("fixture"));
    let state = DeviceState {
      enabled: true,
      pending_conflict_id: None,
      cursor: Some(Cursor {
        revision_id: "fixture-revision".into(),
        snapshot_hash: cloud.fingerprint().unwrap(),
        remote_active: "fixture".into(),
        local_active: "fallback".into(),
        updated_at: String::new(),
      }),
    };
    let selection = pending_preference(&state, &settings).unwrap().to_owned();
    let directory = tempfile::tempdir().unwrap();
    settings.persist(directory.path()).unwrap();
    let waiting = persist_preference_if_ready(
      directory.path(),
      settings.clone(),
      selection.clone(),
      |_| false,
    )
    .unwrap();
    assert!(waiting.is_none());
    assert_eq!(
      Settings::load(directory.path())
        .unwrap()
        .active_connection_id
        .as_deref(),
      Some("fallback")
    );

    let updated =
      persist_preference_if_ready(directory.path(), settings, selection, |connection| {
        connection.connection_id == "fixture"
      })
      .unwrap()
      .unwrap();
    assert_eq!(
      Settings::load(directory.path())
        .unwrap()
        .active_connection_id
        .as_deref(),
      Some("fixture")
    );
    assert!(capture(&updated, &state) == cloud);
    assert!(pending_preference(&state, &updated).is_none());
  }

  #[test]
  fn local_change_status_tracks_only_changes_since_the_saved_cloud_revision() {
    let original = snapshot("original-model");
    let original_hash = original.fingerprint().unwrap();
    let empty = Snapshot::capture(&Settings::default(), None);
    let empty_hash = empty.fingerprint().unwrap();
    let mut device = DeviceState::default();
    assert!(!has_local_changes(&device, &original, &original_hash));

    device.enabled = true;
    assert!(!has_local_changes(&device, &empty, &empty_hash));
    assert!(has_local_changes(&device, &original, &original_hash));

    device.cursor = Some(Cursor {
      revision_id: "fixture-revision".into(),
      snapshot_hash: original_hash.clone(),
      remote_active: LEGACY_DEMO_CONNECTION_ID.into(),
      local_active: LEGACY_DEMO_CONNECTION_ID.into(),
      updated_at: String::new(),
    });
    assert!(!has_local_changes(&device, &original, &original_hash));
    assert!(has_local_changes(&device, &empty, &empty_hash));
    let revised = snapshot("revised-model");
    assert!(has_local_changes(
      &device,
      &revised,
      &revised.fingerprint().unwrap()
    ));
  }

  #[test]
  fn an_old_cursor_keeps_the_cloud_preference_when_no_model_is_active_locally() {
    let mut cloud = snapshot("model");
    cloud.active_connection_id = "fixture".into();
    let (mut local, _) = cloud.settings(&Settings::default()).unwrap();
    assert_eq!(local.active_connection_id.as_deref(), Some("fixture"));
    local.active_connection_id = None;
    let mut state = DeviceState {
      enabled: true,
      pending_conflict_id: None,
      cursor: Some(Cursor {
        revision_id: "fixture-revision".into(),
        snapshot_hash: cloud.fingerprint().unwrap(),
        remote_active: "fixture".into(),
        local_active: LEGACY_DEMO_CONNECTION_ID.into(),
        updated_at: String::new(),
      }),
    };
    assert!(capture(&local, &state) == cloud);
    assert_eq!(
      Snapshot::capture(&local, None).active_connection_id,
      LEGACY_DEMO_CONNECTION_ID
    );
    assert_eq!(pending_preference(&state, &local), Some("fixture"));

    state.enabled = false;
    assert_eq!(pending_preference(&state, &local), None);
    state.enabled = true;

    local.active_connection_id = Some("fixture".into());
    assert_eq!(pending_preference(&state, &local), None);
    local.active_connection_id = None;

    local.connections[0].model_id = "edited-model".into();
    assert_eq!(pending_preference(&state, &local), None);
    local.connections[0].model_id = "model".into();
    assert_eq!(pending_preference(&state, &local), Some("fixture"));

    let cursor = state.cursor.as_mut().unwrap();
    cursor.local_active = "previous-choice".into();
    assert_eq!(pending_preference(&state, &local), None);
  }

  #[test]
  fn review_reports_differences_even_when_regular_sync_would_upload() {
    let local = snapshot("local-edit");
    let cloud = snapshot("cloud-edit");
    let state = DeviceState {
      enabled: true,
      pending_conflict_id: None,
      cursor: Some(Cursor {
        revision_id: "cloud-head".into(),
        snapshot_hash: cloud.fingerprint().unwrap(),
        remote_active: LEGACY_DEMO_CONNECTION_ID.into(),
        local_active: LEGACY_DEMO_CONNECTION_ID.into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
      }),
    };
    let remote = Revision {
      revision_id: "cloud-head".into(),
      parent_revision_ids: vec!["ancestor".into()],
      snapshot: cloud,
      updated_at: "2026-01-01T00:00:00Z".into(),
    };
    let local_hash = local.fingerprint().unwrap();
    assert!(matches!(
      decide(&local, &local_hash, &remote, state.cursor.as_ref(), None),
      Action::Upload
    ));
    let conflict = conflict_id("fixture-account", &remote, &local_hash);
    let mut compared = ComparedSettings {
      head: Some(remote),
      local,
      local_active: None,
      local_hash,
      cloud_apply_blocker: None,
    };
    let status_before = serde_json::to_value(&state).unwrap();
    let reviewed = review_settings("fixture-account", &state, &compared);
    assert_eq!(reviewed["state"], "conflict");
    assert_eq!(reviewed["conflict_id"], conflict);
    assert!(reviewed["cloud_apply_blocker"].is_null());
    assert_eq!(
      reviewed["local"]["connections"][0]["model_id"],
      "local-edit"
    );
    assert_eq!(
      reviewed["cloud"]["connections"][0]["model_id"],
      "cloud-edit"
    );
    compared.cloud_apply_blocker = Some(CloudApplyBlocker::DestinationConflict {
      name: "Local fixture".into(),
    });
    let blocked_review = review_settings("fixture-account", &state, &compared);
    assert_eq!(blocked_review["conflict_id"], conflict);
    assert_eq!(
      blocked_review["cloud_apply_blocker"],
      json!({ "kind": "destination_conflict", "name": "Local fixture" })
    );
    compared.cloud_apply_blocker = Some(CloudApplyBlocker::ConnectionLimit {
      extra_connections: 2,
    });
    assert_eq!(
      review_settings("fixture-account", &state, &compared)["cloud_apply_blocker"],
      json!({ "kind": "connection_limit", "extra_connections": 2 })
    );
    compared.head.as_mut().unwrap().snapshot = compared.local.clone();
    assert_eq!(
      review_settings("fixture-account", &state, &compared)["state"],
      "review_clear"
    );
    compared.head = None;
    assert_eq!(
      review_settings("fixture-account", &state, &compared)["state"],
      "review_clear"
    );
    assert_eq!(serde_json::to_value(&state).unwrap(), status_before);
  }

  #[test]
  fn divergent_edits_and_cloud_removals_require_an_explicit_choice() {
    let local = snapshot("local-edit");
    let mut cursor = Cursor {
      revision_id: "ancestor".into(),
      snapshot_hash: snapshot("original").fingerprint().unwrap(),
      remote_active: LEGACY_DEMO_CONNECTION_ID.into(),
      local_active: LEGACY_DEMO_CONNECTION_ID.into(),
      updated_at: String::new(),
    };
    let mut remote = Revision {
      revision_id: "cloud".into(),
      parent_revision_ids: vec![cursor.revision_id.clone()],
      snapshot: snapshot("cloud-edit"),
      updated_at: String::new(),
    };
    let hash = local.fingerprint().unwrap();
    assert!(matches!(
      decide(&local, &hash, &remote, Some(&cursor), None),
      Action::Conflict
    ));
    assert!(matches!(
      decide(
        &local,
        &hash,
        &remote,
        Some(&cursor),
        Some(Resolution::KeepLocal)
      ),
      Action::Upload
    ));
    assert!(matches!(
      decide(
        &local,
        &hash,
        &remote,
        Some(&cursor),
        Some(Resolution::UseCloud)
      ),
      Action::Download
    ));
    cursor.snapshot_hash.clone_from(&hash);
    assert!(matches!(
      decide(&local, &hash, &remote, Some(&cursor), None),
      Action::Download
    ));
    remote.snapshot = Snapshot::capture(&Settings::default(), None);
    assert!(matches!(
      decide(&local, &hash, &remote, Some(&cursor), None),
      Action::Conflict
    ));
  }

  #[test]
  fn first_publish_requires_a_nonempty_snapshot_and_an_unreviewed_account() {
    let empty = Snapshot::capture(&Settings::default(), None);
    let mut opted_in: DeviceState =
      serde_json::from_value(json!({"enabled":true,"cursor":null})).unwrap();
    assert!(!first_upload_required(&opted_in, &empty).unwrap());
    assert!(first_upload_required(&opted_in, &snapshot("configured-model")).unwrap());
    opted_in.pending_conflict_id = Some("reviewed".into());
    assert!(first_upload_required(&opted_in, &snapshot("configured-model")).is_err());
  }

  #[test]
  fn automatic_download_keeps_a_usable_active_model_when_cloud_has_no_preference() {
    let cloud = snapshot("cloud-model");
    let (mut next, _) = cloud.settings(&Settings::default()).unwrap();
    assert!(next.active_connection_id.is_none());
    retain_usable_active(&mut next, Some("fixture".into()), true, |_| true);
    assert_eq!(next.active_connection_id.as_deref(), Some("fixture"));
    let state = DeviceState {
      enabled: true,
      cursor: Some(Cursor {
        revision_id: "cloud-head".into(),
        snapshot_hash: cloud.fingerprint().unwrap(),
        remote_active: cloud.active_connection_id.clone(),
        local_active: "fixture".into(),
        updated_at: String::new(),
      }),
      pending_conflict_id: None,
    };
    assert!(capture(&next, &state) == cloud);

    next.active_connection_id = None;
    retain_usable_active(&mut next, Some("fixture".into()), true, |_| false);
    assert!(next.active_connection_id.is_none());
    retain_usable_active(&mut next, Some("fixture".into()), false, |_| true);
    assert!(next.active_connection_id.is_none());
  }

  #[test]
  fn an_unready_cloud_preference_only_falls_back_to_the_previous_usable_default() {
    let (mut settings, _) = snapshot("cloud-model")
      .settings(&Settings::default())
      .unwrap();
    let mut fallback = settings.connections[0].clone();
    fallback.connection_id = "fallback".into();
    fallback.credential = credential_ref("fallback");
    settings.connections.push(fallback);
    settings.active_connection_id = Some("fixture".into());
    let ready = |connection: &Connection| connection.connection_id == "fallback";
    retain_usable_active(&mut settings, Some("fallback".into()), true, ready);
    assert_eq!(settings.active_connection_id.as_deref(), Some("fallback"));

    settings.active_connection_id = Some("fixture".into());
    retain_usable_active(&mut settings, None, true, ready);
    assert!(settings.active_connection_id.is_none());
  }

  #[test]
  fn automatic_decision_keeps_differing_reviews_and_provider_swaps_pending() {
    let local = snapshot("local-model");
    let hash = local.fingerprint().unwrap();
    let remote = Revision {
      revision_id: "cloud-head".into(),
      parent_revision_ids: vec!["ancestor".into()],
      snapshot: snapshot("cloud-model"),
      updated_at: String::new(),
    };
    let mut state = DeviceState {
      enabled: true,
      cursor: Some(Cursor {
        revision_id: "ancestor".into(),
        snapshot_hash: hash.clone(),
        remote_active: LEGACY_DEMO_CONNECTION_ID.into(),
        local_active: LEGACY_DEMO_CONNECTION_ID.into(),
        updated_at: String::new(),
      }),
      pending_conflict_id: None,
    };
    assert!(matches!(
      decide_sync(&local, &hash, &remote, &state, None, None),
      Action::Download
    ));
    assert!(matches!(
      decide_sync(
        &local,
        &hash,
        &remote,
        &state,
        None,
        Some(&CloudApplyBlocker::DestinationConflict {
          name: "Local fixture".into()
        })
      ),
      Action::Conflict
    ));
    state.pending_conflict_id = Some("reviewed".into());
    assert!(matches!(
      decide_sync(&local, &hash, &remote, &state, None, None),
      Action::Conflict
    ));
    assert!(matches!(
      decide_sync(
        &local,
        &hash,
        &remote,
        &state,
        Some(Resolution::UseCloud),
        None
      ),
      Action::Download
    ));
  }

  #[test]
  fn capacity_block_goes_to_review_only_when_unresolved_sync_would_download() {
    let local = snapshot("local-model");
    let hash = local.fingerprint().unwrap();
    let remote = Revision {
      revision_id: "cloud-head".into(),
      parent_revision_ids: vec!["ancestor".into()],
      snapshot: snapshot("cloud-model"),
      updated_at: String::new(),
    };
    let mut state = DeviceState {
      enabled: true,
      cursor: Some(Cursor {
        revision_id: "ancestor".into(),
        snapshot_hash: hash.clone(),
        remote_active: LEGACY_DEMO_CONNECTION_ID.into(),
        local_active: LEGACY_DEMO_CONNECTION_ID.into(),
        updated_at: String::new(),
      }),
      pending_conflict_id: None,
    };
    let blocker = CloudApplyBlocker::ConnectionLimit {
      extra_connections: 2,
    };
    assert!(matches!(
      decide_sync(&local, &hash, &remote, &state, None, Some(&blocker)),
      Action::Conflict
    ));
    assert!(matches!(
      decide_sync(
        &local,
        &hash,
        &remote,
        &state,
        Some(Resolution::UseCloud),
        Some(&blocker)
      ),
      Action::Download
    ));

    state
      .cursor
      .as_mut()
      .unwrap()
      .revision_id
      .clone_from(&remote.revision_id);
    assert!(matches!(
      decide_sync(&local, &hash, &remote, &state, None, Some(&blocker)),
      Action::Upload
    ));
    assert!(matches!(
      decide_sync(
        &remote.snapshot,
        &hash,
        &remote,
        &state,
        None,
        Some(&blocker)
      ),
      Action::Current
    ));
  }
}

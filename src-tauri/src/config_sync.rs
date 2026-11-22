mod snapshot;

use crate::{
  account::Access,
  commands::blocking,
  host::Host,
  provider_commands::{commit_settings, finish_snapshot, read_account},
  settings::{Connection, Settings, LEGACY_DEMO_CONNECTION_ID},
};
use apell_core::bundles::{atomic_write, digest};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use snapshot::Snapshot;
use std::{io::Read, path::PathBuf, time::Duration};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

const HEAD_PATH: &str = "/api/v1/config-sync/head";
const MAX_REPLY_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub struct ConfigSync {
  gate: tokio::sync::Mutex<()>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeviceState {
  enabled: bool,
  cursor: Option<Cursor>,
}

#[derive(Deserialize, Serialize)]
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
    let _gate = host.config_sync.gate.lock().await;
    let account_key = host.account.account_key(&app).await?;
    match method.as_str() {
      "sync.status" => Ok(match account_key {
        Some(key) => status(Some(&load_state(&host, &key).await?)),
        None => status(None),
      }),
      "sync.set_enabled" => {
        let input: Enable = input(params)?;
        let key = account_key.ok_or("Sign in to Apell Account to enable sync.")?;
        let mut state = load_state(&host, &key).await?;
        state.enabled = input.enabled;
        let result = status(Some(&state));
        save_state(&host, &key, state).await?;
        Ok(result)
      }
      "sync.run" => {
        let key = account_key.ok_or("Sign in to Apell Account to sync settings.")?;
        let state = load_state(&host, &key).await?;
        if !state.enabled {
          return Err("Turn on config sync first.".into());
        }
        let request: Run = input(params)?;
        if request.resolution.is_some() != request.conflict_id.is_some() {
          return Err("Refresh the conflict before choosing which settings to use.".into());
        }
        let access = host.account.access(&app).await?;
        if access.account_key != key {
          return Err("The signed-in Account changed. Try syncing again.".into());
        }
        synchronize(&app, &host, &access, state, request).await
      }
      _ => Err("Unknown config sync operation.".into()),
    }
  })
  .await
}

fn input<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
  serde_json::from_value(value).map_err(|_| "Invalid config sync request.".into())
}

fn status(state: Option<&DeviceState>) -> Value {
  let Some(state) = state else {
    return json!({"state":"disabled","last_synced_at":null});
  };
  json!({
    "state": if state.enabled { "ready" } else { "off" },
    "last_synced_at": state.cursor.as_ref().map(|cursor| &cursor.updated_at),
  })
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

async fn synchronize(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  mut state: DeviceState,
  input: Run,
) -> Result<Value, String> {
  let head = request_head(host, access, Method::GET, None).await?;
  require_account(app, host, access).await?;
  let (mut local, local_active) = {
    let settings = host.settings.lock().await;
    (
      capture(&settings, &state),
      settings.active_connection_id.clone(),
    )
  };
  local.validate()?;
  let local_hash = local.fingerprint()?;
  let result = match head {
    None if state.cursor.is_some() => {
      return Err("The saved cloud revision is missing. Local settings were kept.".into())
    }
    None if input.resolution.is_some() => {
      return Err("The sync conflict changed. Run sync again.".into())
    }
    None => (
      "uploaded",
      upload(host, access, &local, None).await?,
      local_active,
    ),
    Some(remote) => {
      let conflict = conflict_id(access, &remote, &local_hash);
      if input.conflict_id.as_ref().is_some_and(|id| *id != conflict) {
        return Err("The sync conflict changed. Run sync again.".into());
      }
      match decide(
        &local,
        &local_hash,
        &remote,
        state.cursor.as_ref(),
        input.resolution,
      ) {
        Action::Current => ("current", remote, local_active),
        Action::Upload => {
          let revision = upload(host, access, &local, Some(remote.revision_id)).await?;
          ("uploaded", revision, local_active)
        }
        Action::Download => {
          let active =
            apply_remote(app, host, access, &state, &local_hash, &remote.snapshot).await?;
          ("downloaded", remote, active)
        }
        Action::Conflict => {
          return Ok(json!({
            "state":"conflict",
            "status":status(Some(&state)),
            "conflict_id":conflict,
            "local":local.summary(),
            "cloud":remote.snapshot.summary(),
          }))
        }
      }
    }
  };
  require_account(app, host, access).await?;
  let (outcome, revision, local_active) = result;
  state.cursor = Some(Cursor {
    snapshot_hash: revision.snapshot.fingerprint()?,
    revision_id: revision.revision_id,
    remote_active: revision.snapshot.active_connection_id,
    local_active: local_active.unwrap_or_else(|| LEGACY_DEMO_CONNECTION_ID.to_owned()),
    updated_at: revision.updated_at,
  });
  let output = json!({"state": outcome, "status": status(Some(&state))});
  save_state(host, &access.account_key, state).await?;
  Ok(output)
}

enum Action {
  Current,
  Upload,
  Download,
  Conflict,
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

fn conflict_id(access: &Access, remote: &Revision, local_hash: &str) -> String {
  digest(format!("{}:{}:{local_hash}", access.account_key, remote.revision_id).as_bytes())
}

async fn require_account(app: &AppHandle, host: &Host, access: &Access) -> Result<(), String> {
  if host.account.account_key(app).await?.as_deref() != Some(access.account_key.as_str()) {
    return Err("The signed-in Account changed. Try syncing again.".into());
  }
  Ok(())
}

async fn apply_remote(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  state: &DeviceState,
  local_hash: &str,
  remote: &Snapshot,
) -> Result<Option<String>, String> {
  let _gate = host.credential_gate.lock().await;
  let mut current = host.settings.lock().await;
  require_account(app, host, access).await?;
  if capture(&current, state).fingerprint()? != local_hash {
    return Err("Provider settings changed during sync. Run sync again.".into());
  }
  let (mut next, removals) = remote.settings(&current)?;
  let app = app.clone();
  let root = host.root.clone();
  let previous_active = current.active_connection_id.clone();
  let next = blocking(move || {
    // Keep a usable local default until credentials are entered on this device.
    if let Some(selected) = next.active() {
      if !can_activate(&app, selected)? {
        let previous = previous_active
          .as_deref()
          .and_then(|id| next.connection(id).ok());
        next.active_connection_id = match previous {
          Some(connection) if can_activate(&app, connection)? => previous_active,
          _ => None,
        };
      }
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

fn can_activate(app: &AppHandle, connection: &Connection) -> Result<bool, String> {
  if apell_core::llm::validate_config(&connection.config()).is_err() {
    return Ok(false);
  }
  if matches!(connection.provider_id.as_str(), "custom" | "llama-cpp") {
    return Ok(true);
  }
  Ok(read_account(app, connection)?.is_some())
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
    .provider_http
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
  fn an_old_cursor_keeps_the_cloud_preference_when_no_model_is_active_locally() {
    let mut cloud = snapshot("model");
    cloud.active_connection_id = "fixture".into();
    let (mut local, _) = cloud.settings(&Settings::default()).unwrap();
    assert_eq!(local.active_connection_id.as_deref(), Some("fixture"));
    local.active_connection_id = None;
    let state = DeviceState {
      enabled: true,
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
}

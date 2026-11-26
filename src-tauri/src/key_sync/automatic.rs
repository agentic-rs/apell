mod conflicts;

pub(super) use conflicts::{resolve_conflict, review_conflicts, ConflictResolution};

use super::crypto::{self, Sealed, Secret, MAX_API_KEY};
use super::{
  cloud_context, eligible_endpoint, key_context, local_vault, managed, save_head, BackgroundStop,
  Entry,
};
use crate::{
  commands::blocking,
  host::Host,
  provider_commands::{commit_settings, encode_account, read_account},
  settings::{Connection, CredentialFormat, Settings, MAX_CONNECTIONS},
};
use apell_core::bundles::atomic_write;
use apell_vault_keys::{derive_api_key_encryption_key, derive_sync_fingerprint_key};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::hmac;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
  collections::{BTreeMap, HashSet},
  io::Read,
  path::Path,
  time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{Mutex, Notify};
use uuid::Uuid;
use zeroize::Zeroizing;

const STATE_FILE: &str = "key-sync-device.json";
const PAUSE_FILE: &str = "key-sync-paused";
const SYNC_PAUSED: &str =
  "Apell could not safely track API key changes. Manage local keys in Model connections.";
const MAX_STATE_BYTES: usize = 256 * 1024;
const SYNC_INTERVAL: Duration = Duration::from_secs(60);
const OWNERSHIP_CHANGED: &str = "A credential changed accounts during sync. Try again.";
const FINGERPRINT_V1_DOMAIN: &[u8] = b"apell-api-key-sync-fingerprint-v1";
const FINGERPRINT_V2_DOMAIN: &[u8] = b"apell-api-key-sync-fingerprint-v2";
const FINGERPRINT_V2_PREFIX: &str = "v2:";

#[derive(Default)]
pub(super) struct Automatic {
  wake: Notify,
  health: Mutex<Option<(String, SyncHealth)>>,
  reviews: Mutex<conflicts::Reviews>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum SyncHealth {
  #[default]
  Pending,
  Syncing,
  Current,
  WaitingForModelSettings,
  Conflict,
  Failed(String),
  Paused(String),
}

impl SyncHealth {
  pub(super) fn verified_status(self) -> Value {
    let (phase, error) = match self {
      Self::Pending => ("pending", None),
      Self::Syncing => ("syncing", None),
      Self::Current => ("current", None),
      Self::WaitingForModelSettings => ("waiting_for_model_settings", None),
      Self::Conflict => ("conflict", None),
      Self::Failed(error) => ("failed", Some(error)),
      Self::Paused(error) => ("paused", Some(error)),
    };
    json!({"state": "verified", "sync_state": phase, "sync_error": error})
  }
}

impl Automatic {
  pub(super) async fn clear_conflict_reviews(&self) {
    self.reviews.lock().await.clear();
  }

  pub(super) async fn health(&self, account: &str) -> SyncHealth {
    self
      .health
      .lock()
      .await
      .as_ref()
      .filter(|(owner, _)| owner == account)
      .map(|(_, health)| health.clone())
      .unwrap_or_default()
  }

  pub(super) async fn publish_health(&self, app: &AppHandle, health: Option<(String, SyncHealth)>) {
    let mut current = self.health.lock().await;
    if *current != health {
      *current = health;
      let _ = app.emit_to("main", "apell-key-sync-changed", ());
    }
  }

  pub(super) async fn queue_retry(
    &self,
    app: &AppHandle,
    account: String,
    stop: &BackgroundStop,
  ) -> Result<(), String> {
    let mut current = self.health.lock().await;
    stop.check()?;
    if mark_pending(&mut current, account) {
      let _ = app.emit_to("main", "apell-key-sync-changed", ());
    }
    self.wake.notify_one();
    Ok(())
  }
}

fn mark_pending(current: &mut Option<(String, SyncHealth)>, account: String) -> bool {
  if current.as_ref().is_some_and(|(owner, health)| {
    owner == &account && matches!(health, SyncHealth::Pending | SyncHealth::Syncing)
  }) {
    return false;
  }
  *current = Some((account, SyncHealth::Pending));
  true
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Owner {
  account: String,
  vault_id: Uuid,
}

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", content = "identity", rename_all = "snake_case")]
enum Assignment {
  Paired(Owner),
  Unpaired(String),
  Local,
  #[default]
  Blocked,
}

#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Tracked {
  assignment: Assignment,
  common: Option<String>,
}

impl Tracked {
  fn belongs_to(&self, owner: &Owner) -> bool {
    matches!(&self.assignment, Assignment::Paired(current) if current == owner)
  }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Device {
  initialized: bool,
  connections: BTreeMap<String, Tracked>,
}

impl Device {
  fn load(root: &Path) -> Result<Self, String> {
    match std::fs::symlink_metadata(root.join(PAUSE_FILE)) {
      Ok(_) => return Err(SYNC_PAUSED.into()),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
      Err(_) => return Err("Could not check the device key sync status.".into()),
    }
    let file = match std::fs::File::open(root.join(STATE_FILE)) {
      Ok(file) => file,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
      Err(_) => return Err("Could not read the device key sync state.".into()),
    };
    let mut data = Vec::new();
    file
      .take(MAX_STATE_BYTES as u64 + 1)
      .read_to_end(&mut data)
      .map_err(|_| "Could not read the device key sync state.")?;
    if data.len() > MAX_STATE_BYTES {
      return Err("The device key sync state exceeds its size limit.".into());
    }
    serde_json::from_slice(&data).map_err(|_| "The device key sync state is invalid.".into())
  }

  fn save(&self, root: &Path) -> Result<(), String> {
    let bytes =
      serde_json::to_vec(&self).map_err(|_| "Could not encode the device key sync state.")?;
    if bytes.len() > MAX_STATE_BYTES {
      return Err("The device key sync state exceeds its size limit.".into());
    }
    atomic_write(&root.join(STATE_FILE), &bytes)
  }

  fn record(&mut self, id: String, owner: &Owner, common: Option<String>) {
    self.connections.insert(
      id,
      Tracked {
        assignment: Assignment::Paired(owner.clone()),
        common,
      },
    );
  }

  fn permits(&self, id: &str, owner: &Owner) -> bool {
    self
      .connections
      .get(id)
      .is_none_or(|record| record.belongs_to(owner))
  }

  fn enroll(&mut self, owner: &Owner, ids: impl Iterator<Item = String>) -> bool {
    let mut changed = !self.initialized;
    for id in ids {
      let can_claim = match self.connections.get(&id).map(|record| &record.assignment) {
        Some(Assignment::Unpaired(account)) => account == &owner.account,
        None | Some(Assignment::Local) => !self.initialized,
        _ => false,
      };
      if can_claim {
        self.record(id, owner, None);
        changed = true;
      }
    }
    self.initialized = true;
    changed
  }
}

fn persist_pause(root: &Path) -> Result<(), String> {
  atomic_write(&root.join(PAUSE_FILE), b"paused\n")
    .map_err(|_| "Could not pause encrypted API key sync safely. Free space or check device storage before editing keys.".into())
}

fn load_or_pause(root: &Path) -> Result<Option<Device>, String> {
  match Device::load(root) {
    Ok(device) => Ok(Some(device)),
    Err(_) => {
      // Keep the old ledger. Rebuilding it would lose ownership and deletion history.
      persist_pause(root)?;
      Ok(None)
    }
  }
}

pub(super) async fn require_device_sync(root: &Path) -> Result<(), String> {
  let root = root.to_owned();
  blocking(move || Device::load(&root).map(|_| ())).await
}

pub(crate) fn kick(app: &AppHandle) {
  if let Some(host) = app.try_state::<Host>() {
    host.key_sync.automatic.wake.notify_one();
  }
}

pub(crate) fn start(app: &AppHandle) {
  let app = app.clone();
  tauri::async_runtime::spawn(async move {
    let host = app.state::<Host>();
    let mut interval = tokio::time::interval(SYNC_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
      tokio::select! {
        _ = interval.tick() => (),
        _ = host.key_sync.automatic.wake.notified() => (),
      }
      let _gate = host.key_sync.gate.lock().await;
      let stop = BackgroundStop(host.key_sync.signing_out.subscribe());
      if stop.requested() {
        continue;
      }
      let Ok(Some(account)) = host.account.account_key(&app).await else {
        host.key_sync.automatic.publish_health(&app, None).await;
        continue;
      };
      let health = synchronize(&app, &host, &account, &stop)
        .await
        .unwrap_or_else(SyncHealth::Failed);
      if !stop.requested() {
        host
          .key_sync
          .automatic
          .publish_health(&app, Some((account, health)))
          .await;
      }
    }
  });
}

pub(super) async fn activate(
  app: &AppHandle,
  host: &Host,
  account: &str,
  vault_id: Uuid,
  settings: Settings,
) -> Result<(), String> {
  let owner = Owner {
    account: account.to_owned(),
    vault_id,
  };
  let app = app.clone();
  let root = host.root.clone();
  blocking(move || {
    let Some(mut device) = load_or_pause(&root)? else {
      return Ok(());
    };
    // The first pairing can enroll existing device keys. A later pairing can
    // only adopt new keys recorded for that same account while it was unpaired.
    let eligible = settings
      .connections
      .into_iter()
      .filter(|connection| {
        eligible_endpoint(connection).is_some()
          && local_key(&app, connection).is_ok_and(|key| key.is_some())
      })
      .map(|connection| connection.connection_id);
    if device.enroll(&owner, eligible) && device.save(&root).is_err() {
      persist_pause(&root)?;
    }
    Ok(())
  })
  .await
}

fn local_credential_edits(
  previous: &Settings,
  next: &Settings,
  updates: &[(String, Option<String>)],
) -> BTreeMap<String, bool> {
  let mut edits: BTreeMap<_, _> = next
    .connections
    .iter()
    .filter(|connection| eligible_endpoint(connection).is_some())
    .filter_map(|connection| {
      updates
        .iter()
        .rev()
        .find(|(slot, _)| slot == &connection.credential.account)
        .map(|(_, value)| (connection.connection_id.clone(), value.is_some()))
    })
    .collect();
  for connection in &previous.connections {
    if eligible_endpoint(connection).is_some()
      && next.connection(&connection.connection_id).is_err()
    {
      edits.insert(connection.connection_id.clone(), false);
    }
  }
  edits
}

pub(crate) async fn commit_local_credentials(
  app: &AppHandle,
  host: &Host,
  previous: &Settings,
  next: &Settings,
  updates: Vec<(String, Option<String>)>,
) -> Result<(), String> {
  // The caller holds credential_gate through the sync markers and the local save.
  let edits = local_credential_edits(previous, next, &updates);
  let root = host.root.clone();
  let device = if edits.is_empty() {
    None
  } else {
    let root = root.clone();
    blocking(move || load_or_pause(&root)).await?
  };
  let assignment = if device.is_some() && edits.values().any(|adds_key| *adds_key) {
    match host.account.account_key(app).await {
      Ok(Some(account)) => match local_vault(app, &account)
        .await
        .ok()
        .flatten()
        .filter(|vault| vault.managed && !vault.pending_managed)
      {
        Some(vault) => Assignment::Paired(Owner {
          account,
          vault_id: vault.vault_id,
        }),
        None => Assignment::Unpaired(account),
      },
      Ok(None) => Assignment::Local,
      Err(_) => Assignment::Blocked,
    }
  } else {
    Assignment::Blocked
  };
  let app = app.clone();
  let settings = next.clone();
  blocking(move || {
    commit_tracked_edits(&root, device, edits, assignment, || {
      commit_settings(&app, &root, &settings, updates)
    })
  })
  .await
}

fn commit_tracked_edits(
  root: &Path,
  device: Option<Device>,
  edits: BTreeMap<String, bool>,
  assignment: Assignment,
  commit: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
  let Some(mut device) = device else {
    return commit();
  };
  let blocked: Vec<_> = edits
    .into_iter()
    .filter(|(id, adds_key)| {
      !adds_key
        || match device.connections.get(id) {
          Some(record) => record.assignment != assignment,
          None => assignment != Assignment::Blocked || device.initialized,
        }
    })
    .collect();
  if !blocked.is_empty() {
    for (id, _) in &blocked {
      device.connections.insert(id.clone(), Tracked::default());
    }
    if device.save(root).is_err() {
      persist_pause(root)?;
      return commit();
    }
  }
  commit()?;
  if assignment != Assignment::Blocked && blocked.iter().any(|(_, adds_key)| *adds_key) {
    for (id, adds_key) in blocked {
      if adds_key {
        device.connections.insert(
          id,
          Tracked {
            assignment: assignment.clone(),
            common: None,
          },
        );
      }
    }
    // The device already saved the credential. If enrollment cannot be
    // recorded, the durable unowned marker still prevents misdirected sync.
    let _ = device.save(root);
  }
  Ok(())
}

fn local_key(
  app: &AppHandle,
  connection: &Connection,
) -> Result<Option<Zeroizing<String>>, String> {
  let key = read_account(app, connection)?
    .and_then(|account| account.api_key)
    .map(|key| Zeroizing::new(key.into_inner()))
    .filter(|key| !key.is_empty());
  if key.as_ref().is_some_and(|key| key.len() > MAX_API_KEY) {
    return Err("A saved API key exceeds the sync size limit.".into());
  }
  Ok(key)
}

fn encode_key(connection: &Connection, key: &[u8]) -> Result<String, String> {
  let key = std::str::from_utf8(key).map_err(|_| "An encrypted API key has an invalid format.")?;
  match connection.credential.format {
    CredentialFormat::ApiKey => Ok(key.to_owned()),
    CredentialFormat::Account => encode_account(&apell_core::providers::static_account(
      &connection.provider_id,
      &connection.connection_id,
      Some(key),
    )?),
  }
}

fn fingerprint(secret: &Secret, domain: &[u8], aad: &[u8], value: &[u8]) -> String {
  let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_ref());
  let mut context = hmac::Context::with_key(&key);
  context.update(domain);
  context.update(&(aad.len() as u64).to_be_bytes());
  context.update(aad);
  context.update(value);
  URL_SAFE_NO_PAD.encode(context.sign().as_ref())
}

fn current_fingerprint(key: &Secret, aad: &[u8], value: &[u8]) -> String {
  format!(
    "{FINGERPRINT_V2_PREFIX}{}",
    fingerprint(key, FINGERPRINT_V2_DOMAIN, aad, value)
  )
}

fn compatible_baseline<'a>(
  vault_key: &Secret,
  aad: &[u8],
  previous: &'a str,
  candidates: [Option<(&[u8], &'a str)>; 2],
) -> &'a str {
  if previous.starts_with(FINGERPRINT_V2_PREFIX) {
    return previous;
  }
  // Upgrade a v1 baseline only if a current copy still matches it. An
  // unmatched baseline continues to detect differing edits or a removal.
  for (value, current_hash) in candidates.into_iter().flatten() {
    if fingerprint(vault_key, FINGERPRINT_V1_DOMAIN, aad, value) == previous {
      return current_hash;
    }
  }
  previous
}

#[derive(Debug, PartialEq, Eq)]
enum Merge {
  Current,
  Upload,
  Restore,
  Conflict,
}

fn reconcile(local: Option<&str>, cloud: Option<&str>, common: Option<&str>) -> Merge {
  match (local, cloud) {
    _ if local == cloud => Merge::Current,
    (Some(_), None) if common.is_none() => Merge::Upload,
    (None, Some(_)) if common.is_none() => Merge::Restore,
    (Some(_), Some(_)) if cloud == common => Merge::Upload,
    (Some(_), Some(_)) if local == common => Merge::Restore,
    // Absence with a previous common key can mean an explicit removal.
    // Neither resurrect the removed copy nor clear the surviving one.
    _ => Merge::Conflict,
  }
}

struct VaultKeys<'a> {
  owner: &'a Owner,
  vault: &'a Secret,
  encryption: Secret,
  fingerprint: Secret,
}

impl<'a> VaultKeys<'a> {
  fn new(owner: &'a Owner, vault: &'a Secret) -> Self {
    let vault_id = owner.vault_id.to_string();
    Self {
      owner,
      vault,
      encryption: derive_api_key_encryption_key(vault, &vault_id),
      fingerprint: derive_sync_fingerprint_key(vault, &vault_id),
    }
  }

  fn context(&self, connection: &Connection) -> Result<Vec<u8>, String> {
    key_context(&self.owner.account, self.owner.vault_id, connection)
  }

  fn seal_entry(&self, id: &str, aad: &[u8], value: &[u8]) -> Result<Entry, String> {
    let Sealed { nonce, ciphertext } = crypto::seal_api_key(&self.encryption, aad, value)?;
    Ok(Entry {
      connection_id: id.to_owned(),
      nonce,
      ciphertext,
    })
  }

  fn inspect(
    &self,
    connection: &Connection,
    local: Option<Zeroizing<String>>,
    entry: Option<&Entry>,
    previous: Option<&str>,
  ) -> Result<KeyCopies, String> {
    let aad = self.context(connection)?;
    let cloud = entry
      .map(|entry| crypto::open_api_key(&self.encryption, self.vault, &aad, &entry.sealed()))
      .transpose()?;
    if cloud
      .as_ref()
      .is_some_and(|key| std::str::from_utf8(key).is_err())
    {
      return Err("An encrypted API key has an invalid format.".into());
    }
    let device_hash = local
      .as_ref()
      .map(|key| current_fingerprint(&self.fingerprint, &aad, key.as_bytes()));
    let cloud_hash = cloud
      .as_ref()
      .map(|key| current_fingerprint(&self.fingerprint, &aad, key));
    let baseline = previous.map(|previous| {
      let device_copy = local
        .as_ref()
        .zip(device_hash.as_deref())
        .map(|(key, hash)| (key.as_bytes(), hash));
      let cloud_copy = cloud
        .as_ref()
        .zip(cloud_hash.as_deref())
        .map(|(key, hash)| (key.as_slice(), hash));
      compatible_baseline(self.vault, &aad, previous, [device_copy, cloud_copy])
    });
    let merge = reconcile(device_hash.as_deref(), cloud_hash.as_deref(), baseline);
    Ok(KeyCopies {
      aad,
      device: local,
      cloud,
      device_hash,
      cloud_hash,
      merge,
    })
  }
}

struct KeyCopies {
  aad: Vec<u8>,
  device: Option<Zeroizing<String>>,
  cloud: Option<Zeroizing<Vec<u8>>>,
  device_hash: Option<String>,
  cloud_hash: Option<String>,
  merge: Merge,
}

struct Upload {
  entries: Vec<Entry>,
  common: Vec<(String, String)>,
}

#[derive(Default)]
struct Plan {
  upload: Option<Upload>,
  conflict: bool,
  capacity: bool,
  invalid: bool,
  awaiting_metadata: bool,
}

impl Plan {
  fn health(&self, metadata_error: Option<String>) -> SyncHealth {
    if self.conflict {
      SyncHealth::Conflict
    } else if self.invalid {
      SyncHealth::Failed("Some API keys could not sync. Their saved copies were kept.".into())
    } else if self.capacity {
      SyncHealth::Failed("The encrypted API key limit was reached.".into())
    } else if self.awaiting_metadata {
      match metadata_error {
        Some(error) => SyncHealth::Failed(error),
        None => SyncHealth::WaitingForModelSettings,
      }
    } else {
      SyncHealth::Current
    }
  }
}

async fn synchronize(
  app: &AppHandle,
  host: &Host,
  account: &str,
  stop: &BackgroundStop,
) -> Result<SyncHealth, String> {
  if !host.account.key_exchange_available()
    || !stop
      .wait(local_vault(app, account))
      .await??
      .as_ref()
      .is_some_and(managed::is_verified)
  {
    return Ok(SyncHealth::Pending);
  }
  if let Err(error) = stop.wait(require_device_sync(&host.root)).await? {
    return Ok(SyncHealth::Paused(error));
  }
  host
    .key_sync
    .automatic
    .publish_health(app, Some((account.to_owned(), SyncHealth::Syncing)))
    .await;
  let relay = stop.access(app, host, true, None).await?;
  if relay.account_key != account {
    return Err(OWNERSHIP_CHANGED.into());
  }
  let (vault, access, head) = cloud_context(app, host, &relay, stop).await?;
  let owner = Owner {
    account: account.to_owned(),
    vault_id: vault.vault_id,
  };
  let secret = vault.key()?;
  let expected = head.as_ref().map(|head| head.revision_id);
  let entries: BTreeMap<_, _> = head
    .into_iter()
    .flat_map(|head| head.entries)
    .map(|entry| (entry.connection_id.clone(), entry))
    .collect();
  let missing = stop
    .wait(missing_metadata(host, &owner, &entries))
    .await??;
  let metadata_owner = owner.clone();
  let root = host.root.clone();
  // A metadata retry cannot prevent synchronization of other complete entries.
  let added =
    crate::config_sync::add_credential_connections(app, host, &access, stop, missing, move |ids| {
      retain_restorable(&root, &metadata_owner, ids)
    })
    .await;
  if matches!(added, Ok(true)) {
    let _ = app.emit_to("main", "apell-llm-settings-changed", ());
  }
  let plan_owner = owner.clone();
  let plan_app = app.clone();
  let root = host.root.clone();
  let plan =
    crate::config_sync::apply_key_restores(app, host, &access, stop, move |settings, preferred| {
      apply_plan(
        &plan_app,
        &root,
        settings,
        &plan_owner,
        &secret,
        entries,
        preferred,
      )
    })
    .await?;
  let health = plan.health(added.err());
  if let Some(upload) = plan.upload {
    stop
      .wait(save_head(
        host,
        &access,
        owner.vault_id,
        expected,
        upload.entries,
      ))
      .await??;
    let _gate = stop.wait(host.credential_gate.lock()).await?;
    stop.check()?;
    let root = host.root.clone();
    blocking(move || {
      let mut device = Device::load(&root)?;
      for (id, fingerprint) in upload.common {
        if let Some(record) = device
          .connections
          .get_mut(&id)
          .filter(|record| record.belongs_to(&owner))
        {
          record.common = Some(fingerprint);
        }
      }
      device.save(&root)
    })
    .await?;
  }
  Ok(health)
}

async fn missing_metadata(
  host: &Host,
  owner: &Owner,
  entries: &BTreeMap<String, Entry>,
) -> Result<HashSet<String>, String> {
  let _gate = host.credential_gate.lock().await;
  let settings = host.settings.lock().await;
  let mut ids: HashSet<_> = entries
    .keys()
    .filter(|id| settings.connection(id).is_err())
    .cloned()
    .collect();
  if ids.is_empty() {
    return Ok(HashSet::new());
  }
  let owner = owner.clone();
  let root = host.root.clone();
  blocking(move || {
    retain_restorable(&root, &owner, &mut ids)?;
    Ok(ids)
  })
  .await
}

fn retain_restorable(root: &Path, owner: &Owner, ids: &mut HashSet<String>) -> Result<(), String> {
  let device = Device::load(root)?;
  ids.retain(|id| device.permits(id, owner));
  Ok(())
}

fn apply_plan(
  app: &AppHandle,
  root: &Path,
  settings: &mut Settings,
  owner: &Owner,
  vault_key: &Secret,
  mut entries: BTreeMap<String, Entry>,
  preferred: Option<&str>,
) -> Result<Plan, String> {
  let mut device = Device::load(root)?;
  let mut uploads = Vec::new();
  let mut restores = Vec::new();
  let mut common = Vec::new();
  let mut plan = Plan {
    awaiting_metadata: entries
      .keys()
      .any(|id| settings.connection(id).is_err() && device.permits(id, owner)),
    ..Plan::default()
  };
  let keys = VaultKeys::new(owner, vault_key);
  for connection in &settings.connections {
    if eligible_endpoint(connection).is_none() {
      continue;
    }
    let id = &connection.connection_id;
    let record = device.connections.get(id);
    if !device.permits(id, owner) {
      continue;
    }
    let Ok(local) = local_key(app, connection) else {
      plan.invalid = true;
      continue;
    };
    if record.is_none() && local.is_some() {
      continue;
    }
    let previous = record.and_then(|record| record.common.as_deref());
    let Ok(copies) = keys.inspect(connection, local, entries.get(id), previous) else {
      plan.invalid = true;
      continue;
    };
    match copies.merge {
      Merge::Current => {
        if copies.device_hash.as_deref() != previous {
          common.push((id.clone(), copies.device_hash));
        }
      }
      Merge::Upload if entries.contains_key(id) || entries.len() < MAX_CONNECTIONS => {
        let key = copies
          .device
          .ok_or("The saved API key changed during sync.")?;
        let hash = copies
          .device_hash
          .ok_or("The saved API key changed during sync.")?;
        entries.insert(
          id.clone(),
          keys.seal_entry(id, &copies.aad, key.as_bytes())?,
        );
        uploads.push((id.clone(), hash));
      }
      Merge::Restore => {
        let key = copies
          .cloud
          .ok_or("The cloud API key changed during sync.")?;
        let Ok(value) = encode_key(connection, &key) else {
          plan.invalid = true;
          continue;
        };
        restores.push((connection.credential.account.clone(), Some(value)));
        if record.is_none() {
          device.record(id.clone(), owner, None);
        }
        common.push((id.clone(), copies.cloud_hash));
      }
      Merge::Conflict => plan.conflict = true,
      Merge::Upload => plan.capacity = true,
    }
  }
  if !restores.is_empty() {
    device.save(root)?;
    let mut next = settings.clone();
    if let Some(connection_id) = restored_preference(settings, preferred, &restores) {
      next.active_connection_id = Some(connection_id.to_owned());
    }
    commit_settings(app, root, &next, restores)?;
    *settings = next;
    let _ = app.emit_to("main", "apell-llm-settings-changed", ());
  }
  if !common.is_empty() {
    for (id, hash) in common {
      device.record(id, owner, hash);
    }
    device.save(root)?;
  }
  plan.upload = (!uploads.is_empty()).then(|| Upload {
    entries: entries.into_values().collect(),
    common: uploads,
  });
  Ok(plan)
}

fn restored_preference<'a>(
  settings: &Settings,
  preferred: Option<&'a str>,
  restores: &[(String, Option<String>)],
) -> Option<&'a str> {
  let preferred = preferred?;
  let slot = &settings.connection(preferred).ok()?.credential.account;
  restores
    .iter()
    .any(|(account, value)| account == slot && value.is_some())
    .then_some(preferred)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::settings::credential_ref;

  #[tokio::test]
  async fn automatic_health_is_scoped_to_the_account() {
    let automatic = Automatic::default();
    assert_eq!(automatic.health("first").await, SyncHealth::Pending);
    *automatic.health.lock().await = Some(("first".into(), SyncHealth::Failed("Offline".into())));
    assert_eq!(
      automatic.health("first").await.verified_status(),
      json!({"state":"verified", "sync_state":"failed", "sync_error":"Offline"}),
    );
    assert_eq!(automatic.health("second").await, SyncHealth::Pending);
  }

  #[test]
  fn retry_marks_pending_without_replacing_the_accounts_active_sync() {
    let mut health = Some(("first".into(), SyncHealth::Failed("Offline".into())));
    assert!(mark_pending(&mut health, "first".into()));
    assert_eq!(health, Some(("first".into(), SyncHealth::Pending)));
    assert!(!mark_pending(&mut health, "first".into()));

    health = Some(("first".into(), SyncHealth::Syncing));
    assert!(!mark_pending(&mut health, "first".into()));
    assert_eq!(health, Some(("first".into(), SyncHealth::Syncing)));
    assert!(mark_pending(&mut health, "second".into()));
    assert_eq!(health, Some(("second".into(), SyncHealth::Pending)));
  }

  #[test]
  fn missing_model_metadata_conflicts_and_sync_failures_have_distinct_health() {
    assert_eq!(Plan::default().health(None), SyncHealth::Current);
    let mut plan = Plan {
      awaiting_metadata: true,
      ..Plan::default()
    };
    assert_eq!(
      plan.health(None).verified_status(),
      json!({"state":"verified", "sync_state":"waiting_for_model_settings", "sync_error":null}),
    );
    assert_eq!(
      plan.health(Some("Could not reach model settings".into())),
      SyncHealth::Failed("Could not reach model settings".into())
    );
    plan.conflict = true;
    assert_eq!(plan.health(None), SyncHealth::Conflict);
    assert_eq!(
      SyncHealth::Paused("Ledger unavailable".into()).verified_status(),
      json!({"state":"verified", "sync_state":"paused", "sync_error":"Ledger unavailable"}),
    );
  }

  fn fixture_connection(id: &str, provider: &str) -> Connection {
    Connection {
      connection_id: id.into(),
      name: id.into(),
      model_id: "fixture-model".into(),
      provider_id: provider.into(),
      base_url: apell_core::providers::default_base_url(provider)
        .unwrap()
        .into(),
      credential: credential_ref(id),
    }
  }

  #[test]
  fn replacement_blocks_the_old_restore_before_saving_the_new_key() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let owner = Owner {
      account: "fixture-account".into(),
      vault_id: Uuid::new_v4(),
    };
    let mut device = Device::default();
    device.record("old".into(), &owner, Some("old-fingerprint".into()));
    device.save(root).unwrap();
    let mut requested = HashSet::from(["old".into(), "unrelated".into()]);
    retain_restorable(root, &owner, &mut requested).unwrap();
    assert!(requested.contains("old"));

    let previous = Settings {
      connections: vec![fixture_connection("old", "openai")],
      ..Settings::default()
    };
    let replacement = fixture_connection("new", "deepseek");
    let updates = vec![
      (
        replacement.credential.account.clone(),
        Some("fixture-key".into()),
      ),
      (previous.connections[0].credential.account.clone(), None),
    ];
    let next = Settings {
      connections: vec![replacement],
      ..Settings::default()
    };
    let edits = local_credential_edits(&previous, &next, &updates);
    assert_eq!(
      edits,
      BTreeMap::from([("new".into(), true), ("old".into(), false)])
    );
    commit_tracked_edits(
      root,
      load_or_pause(root).unwrap(),
      edits,
      Assignment::Paired(owner.clone()),
      || {
        // A metadata fetch that started earlier must recheck the on-disk markers.
        retain_restorable(root, &owner, &mut requested)?;
        assert_eq!(requested, HashSet::from(["unrelated".into()]));
        assert!(!Device::load(root)?.permits("new", &owner));
        next.persist(root)
      },
    )
    .unwrap();
    let device = Device::load(root).unwrap();
    assert!(!device.permits("old", &owner));
    assert!(device.connections["new"].belongs_to(&owner));
    assert_eq!(Settings::load(root).unwrap().connections, next.connections);
  }

  #[test]
  fn a_damaged_ledger_allows_local_edits_with_a_durable_device_pause() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let damaged = b"incomplete sync state";
    std::fs::write(root.join(STATE_FILE), damaged).unwrap();
    let next = Settings {
      connections: vec![fixture_connection("new", "deepseek")],
      ..Settings::default()
    };
    commit_tracked_edits(
      root,
      load_or_pause(root).unwrap(),
      BTreeMap::from([("new".into(), true)]),
      Assignment::Blocked,
      || {
        assert_eq!(Device::load(root).err().as_deref(), Some(SYNC_PAUSED));
        next.persist(root)
      },
    )
    .unwrap();
    assert_eq!(Settings::load(root).unwrap().connections, next.connections);
    assert_eq!(std::fs::read(root.join(STATE_FILE)).unwrap(), damaged);

    // Repairing the old file cannot resume sync after local keys have changed.
    std::fs::write(
      root.join(STATE_FILE),
      serde_json::to_vec(&Device::default()).unwrap(),
    )
    .unwrap();
    for account in ["previous-account", "different-account"] {
      let owner = Owner {
        account: account.into(),
        vault_id: Uuid::new_v4(),
      };
      let mut stale = HashSet::from(["old".into()]);
      assert_eq!(
        retain_restorable(root, &owner, &mut stale).unwrap_err(),
        SYNC_PAUSED
      );
    }
    assert!(load_or_pause(root).unwrap().is_none());
  }

  #[test]
  fn a_failed_ownership_checkpoint_still_allows_edits_if_sync_can_be_paused() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let device = load_or_pause(root).unwrap();
    // A directory cannot be replaced by the checkpoint's atomic file rename.
    std::fs::create_dir(root.join(STATE_FILE)).unwrap();
    commit_tracked_edits(
      root,
      device,
      BTreeMap::from([("removed".into(), false)]),
      Assignment::Blocked,
      || {
        assert_eq!(Device::load(root).err().as_deref(), Some(SYNC_PAUSED));
        Ok(())
      },
    )
    .unwrap();
    assert!(root.join(STATE_FILE).is_dir());
  }

  #[test]
  fn a_failed_pause_write_prevents_local_edits_with_unrecorded_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let device = load_or_pause(root).unwrap();
    std::fs::create_dir(root.join(STATE_FILE)).unwrap();
    std::fs::create_dir(root.join(PAUSE_FILE)).unwrap();
    let result = commit_tracked_edits(
      root,
      device,
      BTreeMap::from([("removed".into(), false)]),
      Assignment::Blocked,
      || panic!("the edit requires a durable sync checkpoint or pause"),
    );
    assert!(result.unwrap_err().starts_with("Could not pause"));
  }

  #[test]
  fn removal_blocks_restore_even_without_replacing_a_credential() {
    let previous = Settings {
      connections: vec![fixture_connection("old", "openai")],
      ..Settings::default()
    };
    let mut rename = previous.clone();
    rename.connections[0].name = "Renamed".into();
    assert!(local_credential_edits(&previous, &rename, &[]).is_empty());

    let replacement = Settings {
      connections: vec![fixture_connection("oauth", "codex")],
      ..Settings::default()
    };
    for next in [Settings::default(), replacement] {
      assert_eq!(
        local_credential_edits(&previous, &next, &[]),
        BTreeMap::from([("old".into(), false)])
      );
    }
  }

  #[test]
  fn only_restoring_the_preferred_slots_key_can_activate_it() {
    let mut settings = Settings::default();
    let preferred = fixture_connection("preferred", "openai");
    let unrelated = fixture_connection("unrelated", "openai");
    let preferred_slot = preferred.credential.account.clone();
    let unrelated_slot = unrelated.credential.account.clone();
    settings.connections = vec![preferred, unrelated];

    let restores = vec![
      (unrelated_slot, Some("fixture-key".into())),
      (preferred_slot.clone(), None),
    ];
    assert_eq!(
      restored_preference(&settings, Some("preferred"), &restores),
      None
    );

    let restores = vec![(preferred_slot, Some("fixture-key".into()))];
    assert_eq!(
      restored_preference(&settings, Some("preferred"), &restores),
      Some("preferred")
    );
    assert_eq!(restored_preference(&settings, None, &restores), None);
  }

  #[test]
  fn chooses_missing_values_and_one_sided_rotations_without_resurrecting_deletions() {
    assert_eq!(reconcile(Some("a"), None, None), Merge::Upload);
    assert_eq!(reconcile(None, Some("a"), None), Merge::Restore);
    assert_eq!(reconcile(Some("b"), Some("a"), Some("a")), Merge::Upload);
    assert_eq!(reconcile(Some("a"), Some("b"), Some("a")), Merge::Restore);
    assert_eq!(reconcile(Some("b"), Some("b"), Some("a")), Merge::Current);
    assert_eq!(reconcile(Some("b"), Some("c"), Some("a")), Merge::Conflict);
    assert_eq!(reconcile(Some("a"), Some("b"), None), Merge::Conflict);
    assert_eq!(reconcile(None, Some("a"), Some("a")), Merge::Conflict);
    assert_eq!(reconcile(Some("a"), None, Some("a")), Merge::Conflict);
  }

  #[test]
  fn legacy_fingerprints_preserve_rotation_and_conflict_decisions() {
    let vault_key = crypto::random().unwrap();
    let key = derive_sync_fingerprint_key(&vault_key, &Uuid::new_v4().to_string());
    let aad = b"fixture-context";
    let original = b"original";
    let local_edit = b"local-edit";
    let cloud_edit = b"cloud-edit";
    let legacy = fingerprint(&vault_key, FINGERPRINT_V1_DOMAIN, aad, original);
    let original_hash = current_fingerprint(&key, aad, original);
    let local_hash = current_fingerprint(&key, aad, local_edit);
    let cloud_hash = current_fingerprint(&key, aad, cloud_edit);
    let baseline = |local, cloud| compatible_baseline(&vault_key, aad, &legacy, [local, cloud]);

    let previous = baseline(
      Some((local_edit, &local_hash)),
      Some((original, &original_hash)),
    );
    assert_eq!(
      reconcile(Some(&local_hash), Some(&original_hash), Some(previous)),
      Merge::Upload
    );
    let previous = baseline(
      Some((original, &original_hash)),
      Some((cloud_edit, &cloud_hash)),
    );
    assert_eq!(
      reconcile(Some(&original_hash), Some(&cloud_hash), Some(previous)),
      Merge::Restore
    );
    let previous = baseline(
      Some((local_edit, &local_hash)),
      Some((cloud_edit, &cloud_hash)),
    );
    assert_eq!(previous, legacy);
    assert_eq!(
      reconcile(Some(&local_hash), Some(&cloud_hash), Some(previous)),
      Merge::Conflict
    );
    let previous = baseline(None, Some((original, &original_hash)));
    assert_eq!(
      reconcile(None, Some(&original_hash), Some(previous)),
      Merge::Conflict
    );
  }

  #[test]
  fn device_enrollment_keeps_existing_and_removed_keys_out_of_other_accounts() {
    let first = Owner {
      account: "account-one".into(),
      vault_id: Uuid::new_v4(),
    };
    let second = Owner {
      account: "account-two".into(),
      vault_id: Uuid::new_v4(),
    };
    let mut device = Device::default();
    device
      .connections
      .insert("deleted".into(), Tracked::default());
    for id in ["saved-signed-out", "saved-after-sign-out"] {
      device.connections.insert(
        id.into(),
        Tracked {
          assignment: Assignment::Local,
          common: None,
        },
      );
    }
    device.connections.insert(
      "before-pair".into(),
      Tracked {
        assignment: Assignment::Unpaired(second.account.clone()),
        common: None,
      },
    );
    device.enroll(
      &first,
      [
        "local".into(),
        "deleted".into(),
        "before-pair".into(),
        "saved-signed-out".into(),
      ]
      .into_iter(),
    );
    assert!(device.permits("local", &first));
    assert!(!device.permits("local", &second));
    assert!(!device.permits("deleted", &first));
    assert!(!device.permits("before-pair", &first));
    assert!(device.permits("saved-signed-out", &first));
    device.enroll(
      &second,
      [
        "another-local".into(),
        "before-pair".into(),
        "saved-after-sign-out".into(),
      ]
      .into_iter(),
    );
    assert!(!device.permits("deleted", &second));
    assert!(device.permits("before-pair", &second));
    assert!(!device.permits("saved-after-sign-out", &second));
    assert!(!device.connections.contains_key("another-local"));
  }
}

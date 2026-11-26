use super::crypto::{self, Sealed, Secret, MAX_API_KEY};
use super::{cloud_context, eligible_endpoint, key_context, local_vault, save_head, Entry};
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
const MAX_STATE_BYTES: usize = 256 * 1024;
const SYNC_INTERVAL: Duration = Duration::from_secs(60);
const OWNERSHIP_CHANGED: &str = "A credential changed accounts during sync. Try again.";
const FINGERPRINT_V1_DOMAIN: &[u8] = b"apell-api-key-sync-fingerprint-v1";
const FINGERPRINT_V2_DOMAIN: &[u8] = b"apell-api-key-sync-fingerprint-v2";
const FINGERPRINT_V2_PREFIX: &str = "v2:";

#[derive(Default)]
pub(super) struct Automatic {
  wake: Notify,
  error: Mutex<Option<(String, String)>>,
}

impl Automatic {
  pub(super) async fn error(&self, account: &str) -> Option<String> {
    self
      .error
      .lock()
      .await
      .as_ref()
      .filter(|(owner, _)| owner == account)
      .map(|(_, error)| error.clone())
  }

  async fn publish_error(&self, app: &AppHandle, error: Option<(String, String)>) {
    let mut current = self.error.lock().await;
    if *current != error {
      *current = error;
      let _ = app.emit_to("main", "apell-key-sync-changed", ());
    }
  }
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
    let path = root.join(STATE_FILE);
    atomic_write(&path, &bytes).or_else(|error| {
      if std::fs::read(path).is_ok_and(|published| published == bytes) {
        Ok(())
      } else {
        Err(error)
      }
    })
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
      let Ok(Some(account)) = host.account.account_key(&app).await else {
        host.key_sync.automatic.publish_error(&app, None).await;
        continue;
      };
      let error = synchronize(&app, &host, &account).await.err();
      host
        .key_sync
        .automatic
        .publish_error(&app, error.map(|error| (account, error)))
        .await;
    }
  });
}

pub(super) async fn activate(
  app: &AppHandle,
  host: &Host,
  account: &str,
  vault_id: Uuid,
) -> Result<(), String> {
  let _gate = host.credential_gate.lock().await;
  let settings = host.settings.lock().await.clone();
  let owner = Owner {
    account: account.to_owned(),
    vault_id,
  };
  let app = app.clone();
  let root = host.root.clone();
  blocking(move || {
    let mut device = Device::load(&root)?;
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
    if device.enroll(&owner, eligible) {
      device.save(&root)?;
    }
    Ok(())
  })
  .await
}

pub(crate) async fn commit_local_credentials(
  app: &AppHandle,
  host: &Host,
  settings: &Settings,
  updates: Vec<(String, Option<String>)>,
) -> Result<(), String> {
  // The caller holds credential_gate while these markers and the Keychain
  // update are committed. Record ambiguous ownership before touching a key.
  let edits: Vec<_> = settings
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
  let assignment = if edits.iter().any(|(_, adds_key)| *adds_key) {
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
  let root = host.root.clone();
  let settings = settings.clone();
  blocking(move || {
    if edits.is_empty() {
      return commit_settings(&app, &root, &settings, updates);
    }
    let mut device = Device::load(&root)?;
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
      device.save(&root)?;
    }
    commit_settings(&app, &root, &settings, updates)?;
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
      let _ = device.save(&root);
    }
    Ok(())
  })
  .await
}

pub(crate) async fn suppress_local_restore(
  host: &Host,
  connection: &Connection,
) -> Result<(), String> {
  if eligible_endpoint(connection).is_none() {
    return Ok(());
  }
  let id = connection.connection_id.clone();
  let root = host.root.clone();
  blocking(move || {
    let mut device = Device::load(&root)?;
    device.connections.insert(id, Tracked::default());
    device.save(&root)
  })
  .await
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
}

async fn synchronize(app: &AppHandle, host: &Host, account: &str) -> Result<(), String> {
  if !host.account.key_exchange_available()
    || !local_vault(app, account)
      .await?
      .is_some_and(|vault| vault.managed && !vault.pending_managed)
  {
    return Ok(());
  }
  let relay = host.account.key_exchange_access(app).await?;
  if relay.account_key != account {
    return Err(OWNERSHIP_CHANGED.into());
  }
  let (vault, access, head) = cloud_context(app, host, &relay).await?;
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
  let missing = missing_metadata(host, &owner, &entries).await?;
  // A metadata retry cannot prevent synchronization of other complete entries.
  let added = crate::config_sync::add_credential_connections(app, host, &access, &missing)
    .await
    .unwrap_or(false);
  if added {
    let _ = app.emit_to("main", "apell-llm-settings-changed", ());
  }
  let plan_owner = owner.clone();
  let plan_app = app.clone();
  let root = host.root.clone();
  let (plan, awaiting_metadata) =
    crate::config_sync::apply_key_restores(app, host, &access, move |settings, preferred| {
      let awaiting_metadata = missing.iter().any(|id| settings.connection(id).is_err());
      let plan = apply_plan(
        &plan_app,
        &root,
        settings,
        &plan_owner,
        &secret,
        entries,
        preferred,
      )?;
      Ok((plan, awaiting_metadata))
    })
    .await?;
  if let Some(upload) = plan.upload {
    save_head(app, host, &access, owner.vault_id, expected, upload.entries).await?;
    let _gate = host.credential_gate.lock().await;
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
  if plan.conflict {
    return Err(
      "Some API keys differ between this device and your account. Both copies were kept.".into(),
    );
  }
  if plan.invalid {
    return Err("Some API keys could not sync. Their saved copies were kept.".into());
  }
  if plan.capacity {
    return Err("The encrypted API key limit was reached.".into());
  }
  if awaiting_metadata {
    return Err(
      "Use model settings sync to bring in the provider connections for your backed-up API keys."
        .into(),
    );
  }
  Ok(())
}

async fn missing_metadata(
  host: &Host,
  owner: &Owner,
  entries: &BTreeMap<String, Entry>,
) -> Result<HashSet<String>, String> {
  let _gate = host.credential_gate.lock().await;
  let settings = host.settings.lock().await;
  let ids: Vec<_> = entries
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
    let device = Device::load(&root)?;
    Ok(
      ids
        .into_iter()
        .filter(|id| device.permits(id, &owner))
        .collect(),
    )
  })
  .await
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
  let mut plan = Plan::default();
  let vault_id = owner.vault_id.to_string();
  let encryption_key = derive_api_key_encryption_key(vault_key, &vault_id);
  let fingerprint_key = derive_sync_fingerprint_key(vault_key, &vault_id);
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
    let aad = key_context(&owner.account, owner.vault_id, connection)?;
    let cloud = entries
      .get(id)
      .map(|entry| crypto::open_api_key(&encryption_key, vault_key, &aad, &entry.sealed()))
      .transpose();
    let Ok(cloud) = cloud else {
      plan.invalid = true;
      continue;
    };
    let local_hash = local
      .as_ref()
      .map(|key| current_fingerprint(&fingerprint_key, &aad, key.as_bytes()));
    let cloud_hash = cloud
      .as_ref()
      .map(|key| current_fingerprint(&fingerprint_key, &aad, key));
    let previous = record.and_then(|record| record.common.as_deref());
    let baseline = previous.map(|previous| {
      let local_copy = local
        .as_ref()
        .zip(local_hash.as_deref())
        .map(|(key, hash)| (key.as_bytes(), hash));
      let cloud_copy = cloud
        .as_ref()
        .zip(cloud_hash.as_deref())
        .map(|(key, hash)| (key.as_slice(), hash));
      compatible_baseline(vault_key, &aad, previous, [local_copy, cloud_copy])
    });
    match reconcile(local_hash.as_deref(), cloud_hash.as_deref(), baseline) {
      Merge::Current => {
        if local_hash.as_deref() != previous {
          common.push((id.clone(), local_hash));
        }
      }
      Merge::Upload if entries.contains_key(id) || entries.len() < MAX_CONNECTIONS => {
        let key = local.ok_or("The saved API key changed during sync.")?;
        let hash = local_hash.ok_or("The saved API key changed during sync.")?;
        let Sealed { nonce, ciphertext } =
          crypto::seal_api_key(&encryption_key, &aad, key.as_bytes())?;
        entries.insert(
          id.clone(),
          Entry {
            connection_id: id.clone(),
            nonce,
            ciphertext,
          },
        );
        uploads.push((id.clone(), hash));
      }
      Merge::Restore => {
        let key = cloud.ok_or("The cloud API key changed during sync.")?;
        let Ok(value) = encode_key(connection, &key) else {
          plan.invalid = true;
          continue;
        };
        restores.push((connection.credential.account.clone(), Some(value)));
        if record.is_none() {
          device.record(id.clone(), owner, None);
        }
        common.push((id.clone(), cloud_hash));
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

  #[test]
  fn only_restoring_the_preferred_slots_key_can_activate_it() {
    let mut settings = Settings::default();
    let connection = |id: &str| Connection {
      connection_id: id.into(),
      name: id.into(),
      model_id: "fixture-model".into(),
      provider_id: "openai".into(),
      base_url: apell_core::providers::default_base_url("openai")
        .unwrap()
        .into(),
      credential: credential_ref(id),
    };
    let preferred = connection("preferred");
    let unrelated = connection("unrelated");
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

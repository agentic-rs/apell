//! Shell-only, encrypted export and atomic import of provider connections.
use crate::{
  commands::blocking,
  host::{string, Host},
  provider_commands::{commit_settings, encode_account, finish_snapshot, read_account},
  settings::{Connection, KeyAction, SaveConnection, Settings, MAX_CONNECTIONS},
};
use apell_core::providers;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::{aead, pbkdf2};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value};
use std::{collections::HashSet, num::NonZeroU32};
use tauri::{AppHandle, Manager};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

pub const TRANSFER_PREFIX: &str = "apell-settings:v1:";
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;
pub const MAX_QR_BYTES: usize = 1200;
const MAX_TRANSFER_CONNECTIONS: usize = MAX_CONNECTIONS - 1;
const SALT_BYTES: usize = 16;
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const HEADER_BYTES: usize = SALT_BYTES + NONCE_BYTES;
const MIN_PASSPHRASE_CHARS: usize = 12;
const MAX_PASSPHRASE_BYTES: usize = 256;
const KDF_ITERATIONS: NonZeroU32 = NonZeroU32::new(600_000).unwrap();
static TRANSFER_CRYPTO: Semaphore = Semaphore::const_new(1);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportInput {
  connection_ids: Vec<String>,
  #[serde(deserialize_with = "deserialize_secret")]
  passphrase: Zeroizing<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportInput {
  payload: String,
  #[serde(deserialize_with = "deserialize_secret")]
  passphrase: Zeroizing<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Archive {
  connections: Vec<TransferConnection>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TransferConnection {
  name: String,
  provider_id: String,
  base_url: String,
  model_id: String,
  #[serde(
    default,
    skip_serializing_if = "Option::is_none",
    with = "optional_secret"
  )]
  api_key: Option<Zeroizing<String>>,
}

fn deserialize_secret<'de, D: Deserializer<'de>>(
  deserializer: D,
) -> Result<Zeroizing<String>, D::Error> {
  String::deserialize(deserializer).map(Zeroizing::new)
}

mod optional_secret {
  use super::*;

  pub fn serialize<S: Serializer>(
    secret: &Option<Zeroizing<String>>,
    serializer: S,
  ) -> Result<S::Ok, S::Error> {
    secret
      .as_ref()
      .map(|value| value.as_str())
      .serialize(serializer)
  }

  pub fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
  ) -> Result<Option<Zeroizing<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(|value| value.map(Zeroizing::new))
  }
}

#[derive(Serialize)]
struct PreviewConnection {
  name: String,
  provider_id: String,
  base_url: String,
  model_id: String,
  credential: &'static str,
  duplicate: bool,
}

struct ImportPlan {
  settings: Settings,
  preview: Vec<PreviewConnection>,
  credentials: Vec<(String, Zeroizing<String>)>,
  added: usize,
}

impl TransferConnection {
  fn from_saved(connection: &Connection, api_key: Option<Zeroizing<String>>) -> Self {
    Self {
      name: connection.name.clone(),
      provider_id: connection.provider_id.clone(),
      base_url: connection.base_url.clone(),
      model_id: connection.model_id.clone(),
      api_key,
    }
  }

  fn prepare(&self, scratch: &Settings) -> Result<(Connection, Option<Zeroizing<String>>), String> {
    // The ordinary editor accepts a temporary empty model while drafting. A
    // transfer can only create a complete connection with a remote endpoint.
    if self.model_id.trim().is_empty() || self.base_url.trim().trim_end_matches('/') == "demo" {
      return Err("An exported connection is missing a valid endpoint or model.".into());
    }
    let connection = scratch.prepare_save(&SaveConnection {
      connection_id: None,
      name: self.name.clone(),
      provider_id: self.provider_id.clone(),
      base_url: self.base_url.clone(),
      model_id: self.model_id.clone(),
      key_action: KeyAction::Remove,
    })?;
    let account = self
      .api_key
      .as_ref()
      .map(|key| {
        if connection.is_oauth() {
          return Err("A sign-in connection cannot include a transferred credential.".into());
        }
        if key.is_empty() {
          return Err("An exported API key is invalid.".into());
        }
        let mut account = providers::static_account(
          &connection.provider_id,
          &connection.connection_id,
          Some(key.as_str()),
        )?;
        let encoded = encode_account(&account).map(Zeroizing::new);
        let _key = account
          .api_key
          .take()
          .map(|key| Zeroizing::new(key.into_inner()));
        encoded
      })
      .transpose()?;
    Ok((connection, account))
  }
}

fn validate_passphrase(passphrase: &str) -> Result<(), String> {
  if passphrase.len() > MAX_PASSPHRASE_BYTES || passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
    return Err("Use a passphrase with at least 12 characters and at most 256 UTF-8 bytes.".into());
  }
  Ok(())
}

fn derive_key(passphrase: &str, salt: &[u8]) -> Result<aead::LessSafeKey, String> {
  let mut key = Zeroizing::new([0; 32]);
  pbkdf2::derive(
    pbkdf2::PBKDF2_HMAC_SHA256,
    KDF_ITERATIONS,
    salt,
    passphrase.as_bytes(),
    &mut *key,
  );
  aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &*key)
    .map(aead::LessSafeKey::new)
    .map_err(|_| "Could not prepare transfer encryption.".into())
}

// V1 wire format is the authenticated text prefix followed by unpadded
// base64url of salt (16), nonce (12), ciphertext, and tag (16).
fn seal(archive: &Archive, passphrase: &str) -> Result<String, String> {
  let mut plaintext = Zeroizing::new(
    serde_json::to_vec(archive).map_err(|_| "Could not encode the settings export.")?,
  );
  let encoded_bytes = (HEADER_BYTES + plaintext.len() + TAG_BYTES) * 4;
  if TRANSFER_PREFIX.len() + encoded_bytes.div_ceil(3) > MAX_PAYLOAD_BYTES {
    return Err("This settings export is too large. Select fewer connections.".into());
  }
  let mut header = [0u8; HEADER_BYTES];
  getrandom::fill(&mut header).map_err(|_| "Could not create a secure settings export.")?;
  let (salt, nonce) = header.split_at(SALT_BYTES);
  let key = derive_key(passphrase, salt)?;
  key
    .seal_in_place_append_tag(
      aead::Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| "Could not prepare transfer encryption.")?,
      aead::Aad::from(TRANSFER_PREFIX.as_bytes()),
      &mut *plaintext,
    )
    .map_err(|_| "Could not encrypt the settings export.")?;
  let mut envelope = Vec::with_capacity(HEADER_BYTES + plaintext.len());
  envelope.extend_from_slice(&header);
  envelope.extend_from_slice(&plaintext);
  Ok(format!(
    "{TRANSFER_PREFIX}{}",
    URL_SAFE_NO_PAD.encode(envelope)
  ))
}

fn decode_envelope(payload: &str) -> Result<Vec<u8>, String> {
  if payload.len() > MAX_PAYLOAD_BYTES {
    return Err("This settings export exceeds the size limit.".into());
  }
  let encoded = payload
    .strip_prefix(TRANSFER_PREFIX)
    .ok_or("This is not a supported Apell settings export.")?;
  let bytes = URL_SAFE_NO_PAD
    .decode(encoded)
    .map_err(|_| "This settings export is malformed.")?;
  if bytes.len() <= HEADER_BYTES + TAG_BYTES {
    return Err("This settings export is malformed.".into());
  }
  Ok(bytes)
}

fn open(envelope: Vec<u8>, passphrase: &str) -> Result<Archive, String> {
  let mut envelope = Zeroizing::new(envelope);
  let (header, ciphertext) = envelope.split_at_mut(HEADER_BYTES);
  let (salt, nonce) = header.split_at(SALT_BYTES);
  let plaintext = derive_key(passphrase, salt)?
    .open_in_place(
      aead::Nonce::try_assume_unique_for_key(nonce)
        .map_err(|_| "This settings export is malformed.")?,
      aead::Aad::from(TRANSFER_PREFIX.as_bytes()),
      ciphertext,
    )
    .map_err(|_| "The passphrase is incorrect or the settings export was changed.")?;
  serde_json::from_slice(plaintext)
    .map_err(|_| "This settings export contains invalid data.".into())
}

async fn crypto<T: Send + 'static>(
  work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
  let permit = TRANSFER_CRYPTO
    .try_acquire()
    .map_err(|_| "A settings transfer is already being processed. Try again.")?;
  blocking(move || {
    let _permit = permit;
    work()
  })
  .await
}

fn check_count(count: usize) -> Result<(), String> {
  if !(1..=MAX_TRANSFER_CONNECTIONS).contains(&count) {
    return Err("A settings transfer must include between 1 and 31 connections.".into());
  }
  Ok(())
}

fn duplicate(existing: &Connection, candidate: &Connection) -> bool {
  existing.provider_id == candidate.provider_id
    && existing.name.trim() == candidate.name
    && existing.base_url.trim().trim_end_matches('/') == candidate.base_url
    && existing.model_id.trim() == candidate.model_id
}

fn plan_import(current: &Settings, archive: Archive) -> Result<ImportPlan, String> {
  check_count(archive.connections.len())?;
  let mut plan = ImportPlan {
    settings: current.clone(),
    preview: Vec::with_capacity(archive.connections.len()),
    credentials: Vec::new(),
    added: 0,
  };
  // Validate incoming metadata independently from capacity so re-importing
  // duplicates succeeds even when all connection slots are occupied.
  let scratch = Settings::default();
  for entry in archive.connections {
    let (connection, account) = entry.prepare(&scratch)?;
    let duplicate = plan
      .settings
      .connections
      .iter()
      .any(|existing| duplicate(existing, &connection));
    plan.preview.push(PreviewConnection {
      name: connection.name.clone(),
      provider_id: connection.provider_id.clone(),
      base_url: connection.base_url.clone(),
      model_id: connection.model_id.clone(),
      credential: if connection.is_oauth() {
        "sign_in"
      } else if account.is_some() {
        "api_key"
      } else {
        "none"
      },
      duplicate,
    });
    if duplicate {
      continue;
    }
    if plan.settings.connections.len() >= MAX_CONNECTIONS {
      return Err(
        "There is not enough space to import all connections. Apell can save up to 31.".into(),
      );
    }
    if let Some(account) = account {
      plan
        .credentials
        .push((connection.credential.account.clone(), account));
    }
    plan.settings.replace(None, connection);
    plan.added += 1;
  }
  Ok(plan)
}

async fn export_archive(app: AppHandle, connection_ids: Vec<String>) -> Result<Archive, String> {
  check_count(connection_ids.len())?;
  let mut selected = HashSet::new();
  if !connection_ids.iter().all(|id| selected.insert(id)) {
    return Err("Choose each connection only once.".into());
  }
  finish_snapshot(async move {
    let host = app.state::<Host>();
    let _gate = host.credential_gate.lock().await;
    let saved = {
      let settings = host.settings.lock().await;
      connection_ids
        .iter()
        .map(|id| settings.connection(id).cloned())
        .collect::<Result<Vec<_>, _>>()?
    };
    let app = app.clone();
    blocking(move || {
      let scratch = Settings::default();
      let mut connections = Vec::with_capacity(saved.len());
      for connection in saved {
        if connection.is_demo() {
          return Err("Local demo does not need to be exported.".into());
        }
        // Device credentials are never read for transfer, even for old
        // settings that happen to have a credential in the vault.
        let api_key = if connection.is_oauth() {
          None
        } else {
          read_account(&app, &connection)?
            .and_then(|account| account.api_key)
            .map(|key| Zeroizing::new(key.into_inner()))
        };
        let entry = TransferConnection::from_saved(&connection, api_key);
        entry.prepare(&scratch)?;
        connections.push(entry);
      }
      Ok(Archive { connections })
    })
    .await
  })
  .await
}

async fn apply_archive(app: AppHandle, archive: Archive) -> Result<Value, String> {
  finish_snapshot(async move {
    let host = app.state::<Host>();
    let _gate = host.credential_gate.lock().await;
    let mut current = host.settings.lock().await;
    let plan = plan_import(&current, archive)?;
    let skipped = plan.preview.len() - plan.added;
    if plan.added != 0 {
      let app = app.clone();
      let root = host.root.clone();
      let settings = plan.settings.clone();
      blocking(move || {
        let updates = plan
          .credentials
          .into_iter()
          .map(|(slot, mut credential)| (slot, Some(std::mem::take(&mut *credential))))
          .collect();
        commit_settings(&app, &root, &settings, updates)
      })
      .await?;
      *current = plan.settings;
    }
    Ok(json!({"added": plan.added, "skipped": skipped}))
  })
  .await
}

pub async fn dispatch(
  app: AppHandle,
  host: &Host,
  method: &str,
  params: Value,
) -> Result<Value, String> {
  match method {
    "settings_transfer.export" => {
      let input: ExportInput =
        serde_json::from_value(params).map_err(|_| "Invalid settings export request.")?;
      validate_passphrase(&input.passphrase)?;
      let archive = export_archive(app, input.connection_ids).await?;
      let payload = crypto(move || seal(&archive, &input.passphrase)).await?;
      let qr_available = payload.len() <= MAX_QR_BYTES;
      Ok(json!({"payload": payload, "qr_available": qr_available}))
    }
    "settings_transfer.preview" | "settings_transfer.apply" => {
      // Reject oversized data before deserializing or doing any key work.
      if string(&params, "payload")?.len() > MAX_PAYLOAD_BYTES {
        return Err("This settings export exceeds the size limit.".into());
      }
      let input: ImportInput =
        serde_json::from_value(params).map_err(|_| "Invalid settings import request.")?;
      validate_passphrase(&input.passphrase)?;
      let envelope = decode_envelope(&input.payload)?;
      let archive = crypto(move || open(envelope, &input.passphrase)).await?;
      if method == "settings_transfer.apply" {
        apply_archive(app, archive).await
      } else {
        let current = host.settings.lock().await;
        let plan = plan_import(&current, archive)?;
        Ok(json!({"connections": plan.preview}))
      }
    }
    _ => Err("Unknown settings transfer operation.".into()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const PASSPHRASE: &str = "fixture passphrase 17";
  const API_KEY: &str = "fixture-only-credential";

  fn connection(name: &str, api_key: Option<&str>) -> TransferConnection {
    TransferConnection {
      name: name.into(),
      provider_id: "custom".into(),
      base_url: "https://example.invalid/v1".into(),
      model_id: "fixture-model".into(),
      api_key: api_key.map(|value| Zeroizing::new(value.into())),
    }
  }

  fn archive(connections: Vec<TransferConnection>) -> Archive {
    Archive { connections }
  }

  #[test]
  fn encrypted_transfer_round_trips_and_rejects_tampering() {
    let archive = archive(vec![connection("Fixture profile", Some(API_KEY))]);
    let payload = seal(&archive, PASSPHRASE).unwrap();
    assert!(payload.starts_with(TRANSFER_PREFIX));
    assert!(!payload.contains(API_KEY));
    assert!(!payload.contains("Fixture profile"));
    assert!(payload.len() <= MAX_QR_BYTES);
    let other = seal(&archive, PASSPHRASE).unwrap();
    assert_ne!(payload, other);

    let envelope = decode_envelope(&payload).unwrap();
    let decoded = open(envelope.clone(), PASSPHRASE).unwrap();
    assert_eq!(
      decoded.connections[0]
        .api_key
        .as_deref()
        .map(String::as_str),
      Some(API_KEY)
    );
    let mut corrupted = envelope.clone();
    let last = corrupted.last_mut().unwrap();
    *last ^= 1;
    assert!(open(corrupted, PASSPHRASE).is_err());
    assert!(open(envelope, "wrong fixture passphrase").is_err());
  }

  #[test]
  fn invalid_or_oversized_wire_data_is_rejected_before_key_work() {
    for payload in [
      "apell-settings:v2:AAAA",
      "apell-settings:v1:%%%%",
      "apell-settings:v1:AAAA",
    ] {
      assert!(decode_envelope(payload).is_err());
    }
    let oversized = format!("{TRANSFER_PREFIX}{}", "A".repeat(MAX_PAYLOAD_BYTES));
    assert!(decode_envelope(&oversized).is_err());
    assert!(validate_passphrase("short").is_err());
    assert!(validate_passphrase(&"a".repeat(MAX_PASSPHRASE_BYTES + 1)).is_err());
    assert!(validate_passphrase("模拟配置传输口令只用于验证").is_ok());
  }

  #[test]
  fn merge_skips_existing_connections_and_scopes_new_keys() {
    let mut original = Settings::default();
    let (existing, _) = connection("Existing", None).prepare(&original).unwrap();
    let active = existing.connection_id.clone();
    let old_slot = existing.credential.account.clone();
    original.replace(None, existing);
    original.active_connection_id = active.clone();

    let mut repeated = connection(" Existing ", Some("fixture-replacement"));
    repeated.base_url.push('/');
    let plan = plan_import(
      &original,
      archive(vec![
        repeated,
        connection("New", Some(API_KEY)),
        connection("New", Some(API_KEY)),
      ]),
    )
    .unwrap();
    assert_eq!(plan.added, 1);
    assert_eq!(plan.credentials.len(), 1);
    assert_eq!(plan.settings.active_connection_id, active);
    assert_eq!(
      plan
        .settings
        .connection(&active)
        .unwrap()
        .credential
        .account,
      old_slot
    );
    assert_eq!(plan.settings.connections.len(), 3);

    let saved = &plan.settings.connections[2];
    let (slot, encoded) = &plan.credentials[0];
    assert_eq!(slot, &saved.credential.account);
    assert_ne!(slot, &old_slot);
    let account: Value = serde_json::from_str(encoded).unwrap();
    assert_eq!(account["api_key"], API_KEY);
    assert_eq!(account["id"], saved.connection_id);
    assert_eq!(account["provider"], "openai");

    let preview = json!(plan.preview);
    assert_eq!(preview[0]["duplicate"], true);
    assert_eq!(preview[1]["credential"], "api_key");
    assert_eq!(preview[2]["duplicate"], true);
    let preview = preview.to_string();
    assert!(!preview.contains(API_KEY));
    assert!(!preview.contains(&saved.connection_id));
    assert!(!preview.contains("fixture-replacement"));
  }

  #[test]
  fn transfer_rejects_oauth_tokens_and_invalid_destinations() {
    let mut oauth = connection("Sign in", None);
    oauth.provider_id = "codex".into();
    oauth.base_url = providers::default_base_url("codex").unwrap().into();
    let plan = plan_import(&Settings::default(), archive(vec![oauth])).unwrap();
    assert!(plan.credentials.is_empty());
    assert_eq!(json!(plan.preview)[0]["credential"], "sign_in");

    let mut oauth_with_key = connection("Sign in", Some(API_KEY));
    oauth_with_key.provider_id = "codex".into();
    oauth_with_key.base_url = providers::default_base_url("codex").unwrap().into();
    let mut invalid = connection("Invalid", None);
    invalid.base_url = "demo".into();
    let mut missing_model = connection("Missing model", None);
    missing_model.model_id.clear();
    for forbidden in [oauth_with_key, invalid, missing_model] {
      assert!(plan_import(
        &Settings::default(),
        archive(vec![connection("Valid", None), forbidden])
      )
      .is_err());
    }
    assert!(serde_json::from_value::<Archive>(json!({
      "connections": [{
        "name": "Fixture",
        "base_url": "https://example.invalid/v1",
        "provider_id": "custom",
        "model_id": "fixture",
        "credential": {"account": "forbidden"}
      }]
    }))
    .is_err());
  }

  #[test]
  fn imports_at_capacity_allow_duplicates_but_reject_new_connections() {
    let seed = Settings::default();
    let full = plan_import(
      &seed,
      archive(
        (0..MAX_TRANSFER_CONNECTIONS)
          .map(|index| connection(&format!("Fixture {index}"), None))
          .collect(),
      ),
    )
    .unwrap()
    .settings;
    let repeat = plan_import(&full, archive(vec![connection("Fixture 0", Some(API_KEY))])).unwrap();
    assert_eq!(repeat.added, 0);
    assert!(repeat.credentials.is_empty());
    assert!(plan_import(&full, archive(vec![connection("Extra", None)])).is_err());
  }
}

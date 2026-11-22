mod crypto;

use crate::{
  account::Access,
  commands::blocking,
  host::Host,
  provider_commands::{commit_settings, encode_account, finish_snapshot, read_account},
  settings::{Connection, CredentialFormat, MAX_CONNECTIONS},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use crypto::{PairCode, Sealed, Secret, MAX_API_KEY};
use reqwest::{Client, Method, Response, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
  collections::{BTreeMap, HashSet},
  time::Duration,
};
use tauri::{AppHandle, Manager};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::{sync::Mutex, time::Instant};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const KEY_VAULT: &str = "/api/v1/key-vault";
const CLOUD_KEYS: &str = "/api/v1/credential-sync/head";
const PAIR_LIFETIME: Duration = Duration::from_secs(300);
const MAX_REPLY: usize = 192 * 1024;
const LOCKED: &str = "Unlock encrypted API key sync on this device first.";
const EXPIRED: &str = "This pairing request expired. Start again on the new device.";

pub(crate) struct KeySync {
  client: Client,
  pub gate: Mutex<()>,
  pending: Mutex<Option<PendingPair>>,
}

struct PendingPair {
  account: String,
  code: PairCode,
  deadline: Instant,
}

impl PendingPair {
  fn matches(&self, account: &str, code: &PairCode) -> bool {
    self.account == account && self.code.id == code.id && *self.code.secret == *code.secret
  }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LocalVault {
  vault_id: Uuid,
  key: String,
  setup: Option<PendingSetup>,
}

impl LocalVault {
  fn new(vault_id: Uuid, key: &Secret) -> Self {
    Self {
      vault_id,
      key: URL_SAFE_NO_PAD.encode(key.as_ref()),
      setup: None,
    }
  }

  fn key(&self) -> Result<Secret, String> {
    crypto::decode_key(&self.key).ok_or_else(|| "The saved encryption key is invalid.".into())
  }
}

impl Drop for LocalVault {
  fn drop(&mut self) {
    self.key.zeroize();
  }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingSetup {
  recovery_key: String,
  recovery: Sealed,
}

impl Drop for PendingSetup {
  fn drop(&mut self) {
    self.recovery_key.zeroize();
  }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Vault {
  vault_id: Uuid,
  recovery: Sealed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultReply {
  vault: Option<Vault>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairReply {
  vault_id: Uuid,
  envelope: Option<Sealed>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Entry {
  connection_id: String,
  nonce: String,
  ciphertext: String,
}

impl Entry {
  fn sealed(&self) -> Sealed {
    Sealed {
      nonce: self.nonce.clone(),
      ciphertext: self.ciphertext.clone(),
    }
  }
}

#[derive(Deserialize)]
struct CloudHead {
  revision_id: Uuid,
  vault_id: Uuid,
  entries: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HeadReply {
  head: Option<CloudHead>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryInput {
  recovery_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairInput {
  pairing_code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadInput {
  connection_ids: Vec<String>,
  #[serde(default)]
  replace_existing: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveInput {
  expected_head: Uuid,
  connection_ids: Vec<String>,
}

impl KeySync {
  pub fn new() -> Result<Self, String> {
    let client = apell_core::providers::http_client()?;
    Ok(Self {
      client,
      gate: Mutex::new(()),
      pending: Mutex::new(None),
    })
  }

  pub async fn clear_pair(&self) {
    *self.pending.lock().await = None;
  }

  async fn request(
    &self,
    access: &Access,
    method: Method,
    path: &str,
    body: Option<Value>,
  ) -> Result<Response, String> {
    let url = access
      .sync_url
      .join(path)
      .map_err(|_| "Invalid key sync address.")?;
    if url.scheme() != "https" {
      return Err("Invalid key sync address.".into());
    }
    let mut request = self
      .client
      .request(method, url)
      .bearer_auth(access.token.as_str())
      .timeout(Duration::from_secs(20));
    if let Some(body) = body {
      request = request.json(&body);
    }
    request
      .send()
      .await
      .map_err(|_| "Could not reach encrypted API key sync. Try again.".into())
  }

  async fn vault(&self, access: &Access) -> Result<Option<Vault>, String> {
    let response = self.request(access, Method::GET, KEY_VAULT, None).await?;
    Ok(decode::<VaultReply>(response).await?.vault)
  }

  async fn pair(&self, access: &Access, path: &str) -> Result<PairReply, String> {
    let response = self.request(access, Method::GET, path, None).await?;
    decode(response).await
  }

  async fn head(&self, access: &Access, vault_id: Uuid) -> Result<Option<CloudHead>, String> {
    let response = self.request(access, Method::GET, CLOUD_KEYS, None).await?;
    let head = decode::<HeadReply>(response).await?.head;
    if let Some(head) = &head {
      if head.vault_id != vault_id {
        return Err("The cloud API keys belong to a different encryption vault.".into());
      }
      let entries = &head.entries;
      if entries.len() > MAX_CONNECTIONS
        || entries
          .windows(2)
          .any(|pair| pair[0].connection_id >= pair[1].connection_id)
      {
        return Err("The cloud API key list is invalid.".into());
      }
    }
    Ok(head)
  }
}

async fn decode<T: DeserializeOwned>(mut response: Response) -> Result<T, String> {
  if response.status() != StatusCode::OK {
    return Err(response_error(response.status()));
  }
  let mut body = Vec::new();
  while let Some(chunk) = response
    .chunk()
    .await
    .map_err(|_| "Could not read encrypted API key sync.")?
  {
    if chunk.len() > MAX_REPLY.saturating_sub(body.len()) {
      return Err("The encrypted API key response exceeds its size limit.".into());
    }
    body.extend_from_slice(&chunk);
  }
  serde_json::from_slice(&body).map_err(|_| "Encrypted API key sync returned invalid data.".into())
}

fn response_error(status: StatusCode) -> String {
  match status {
    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
      "This Account is not authorized for encrypted API key sync.".into()
    }
    StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => {
      "Another device updated encrypted API key sync. Refresh and try again.".into()
    }
    StatusCode::NOT_FOUND | StatusCode::GONE => EXPIRED.into(),
    _ => "Encrypted API key sync is unavailable. Try again later.".into(),
  }
}

fn acknowledged(response: Response) -> Result<(), String> {
  if response.status() == StatusCode::NO_CONTENT {
    Ok(())
  } else {
    Err(response_error(response.status()))
  }
}

fn slot(account: &str) -> String {
  format!("apell-api-key-vault-v1-{account}")
}

async fn local_vault(app: &AppHandle, account: &str) -> Result<Option<LocalVault>, String> {
  let app = app.clone();
  let slot = slot(account);
  blocking(move || {
    let Some(value) = tauri_plugin_apell_credentials::read(&app, &slot)?.map(Zeroizing::new) else {
      return Ok(None);
    };
    serde_json::from_str(&value)
      .map(Some)
      .map_err(|_| "The saved encryption vault is invalid.".into())
  })
  .await
}

async fn save_vault(app: &AppHandle, account: &str, vault: &LocalVault) -> Result<(), String> {
  let app = app.clone();
  let slot = slot(account);
  let data = serde_json::to_string(vault)
    .map(Zeroizing::new)
    .map_err(|_| "Could not encode the encryption vault.")?;
  blocking(move || tauri_plugin_apell_credentials::write(&app, &slot, Some(&data))).await
}

async fn verify_account(app: &AppHandle, host: &Host, account: &str) -> Result<(), String> {
  if host.account.account_key(app).await?.as_deref() != Some(account) {
    return Err("The signed-in Account changed. Try again.".into());
  }
  Ok(())
}

async fn status(app: &AppHandle, host: &Host) -> Result<Value, String> {
  if !host.account.key_exchange_available() {
    return Ok(json!({ "state": "unavailable" }));
  }
  let Some(account) = host.account.account_key(app).await? else {
    return Ok(json!({ "state": "signed_out" }));
  };
  let access = host.account.key_exchange_access(app).await?;
  if access.account_key != account {
    return Err("The signed-in Account changed. Try again.".into());
  }
  let remote = host.key_sync.vault(&access).await?;
  let local = local_vault(app, &account).await?;
  let state = match (remote, local) {
    (None, _) => "setup",
    (Some(remote), Some(local)) if remote.vault_id == local.vault_id => {
      if local.setup.is_some() {
        "setup"
      } else {
        "unlocked"
      }
    }
    _ => "locked",
  };
  Ok(json!({ "state": state }))
}

async fn create(app: &AppHandle, host: &Host, access: &Access) -> Result<Value, String> {
  let remote = host.key_sync.vault(access).await?;
  let stored = local_vault(app, &access.account_key).await?;
  let vault = match stored {
    Some(saved) if saved.setup.is_some() => saved,
    _ if remote.is_some() => {
      return Err("This Account already has encrypted API key sync. Unlock it to continue.".into())
    }
    _ => {
      let vault_id = Uuid::new_v4();
      let secret = crypto::random()?;
      let recovery = crypto::random()?;
      let aad = crypto::context("recovery", &access.account_key, vault_id, &[])?;
      let mut vault = LocalVault::new(vault_id, &secret);
      vault.setup = Some(PendingSetup {
        recovery_key: crypto::recovery_code(&recovery),
        recovery: crypto::seal(&recovery, &aad, secret.as_ref())?,
      });
      verify_account(app, host, &access.account_key).await?;
      save_vault(app, &access.account_key, &vault).await?;
      vault
    }
  };
  let setup = vault
    .setup
    .as_ref()
    .ok_or("Encrypted API key sync is already set up.")?;
  if let Some(remote) = remote {
    if remote.vault_id != vault.vault_id || remote.recovery != setup.recovery {
      return Err(
        "Another device set up encrypted API key sync. Unlock the existing vault.".into(),
      );
    }
  }
  verify_account(app, host, &access.account_key).await?;
  Ok(json!({ "recovery_key": setup.recovery_key }))
}

async fn confirm_recovery(app: &AppHandle, host: &Host, access: &Access) -> Result<Value, String> {
  let mut local = local_vault(app, &access.account_key).await?.ok_or(LOCKED)?;
  let remote = host.key_sync.vault(access).await?;
  if remote
    .as_ref()
    .is_some_and(|remote| remote.vault_id != local.vault_id)
  {
    return Err("Another device set up encrypted API key sync. Unlock the existing vault.".into());
  }
  if let Some(setup) = &local.setup {
    if let Some(remote) = remote {
      if setup.recovery != remote.recovery {
        return Err("The saved recovery key does not match this vault.".into());
      }
    } else {
      // Publish only after the user has saved the recovery key. The local
      // pending record makes this exact write retryable after a lost response.
      verify_account(app, host, &access.account_key).await?;
      let response = host
        .key_sync
        .request(
          access,
          Method::PUT,
          KEY_VAULT,
          Some(json!({
            "vault_id": local.vault_id,
            "recovery": setup.recovery,
          })),
        )
        .await?;
      acknowledged(response)?;
    }
    local.setup = None;
    verify_account(app, host, &access.account_key).await?;
    save_vault(app, &access.account_key, &local).await?;
  } else if remote.is_none() {
    return Err(LOCKED.into());
  }
  Ok(Value::Null)
}

async fn recover(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  input: RecoveryInput,
) -> Result<Value, String> {
  let key = crypto::recovery_key(&Zeroizing::new(input.recovery_key))?;
  let remote = host.key_sync.vault(access).await?.ok_or(LOCKED)?;
  let context = crypto::context("recovery", &access.account_key, remote.vault_id, &[])?;
  let secret = crypto::open_vault(&key, &context, &remote.recovery)?;
  verify_account(app, host, &access.account_key).await?;
  save_vault(
    app,
    &access.account_key,
    &LocalVault::new(remote.vault_id, &secret),
  )
  .await?;
  Ok(Value::Null)
}

fn pair_path(id: Uuid) -> String {
  format!("{KEY_VAULT}/pairs/{id}")
}

fn pair_context(account: &str, vault_id: Uuid, pair_id: Uuid) -> Result<Vec<u8>, String> {
  crypto::context("pair", account, vault_id, &[&pair_id.to_string()])
}

async fn pair_start(host: &Host, access: &Access) -> Result<Value, String> {
  let vault = host.key_sync.vault(access).await?.ok_or(LOCKED)?;
  let code = PairCode::new()?;
  let response = host
    .key_sync
    .request(
      access,
      Method::PUT,
      &pair_path(code.id),
      Some(json!({
        "vault_id": vault.vault_id
      })),
    )
    .await?;
  acknowledged(response)?;
  let expires_at = (OffsetDateTime::now_utc() + PAIR_LIFETIME)
    .format(&Rfc3339)
    .map_err(|_| "Could not encode pairing expiry.")?;
  let display = json!({ "pairing_code": code.encode(), "expires_at": expires_at });
  *host.key_sync.pending.lock().await = Some(PendingPair {
    account: access.account_key.clone(),
    code,
    deadline: Instant::now() + PAIR_LIFETIME,
  });
  Ok(display)
}

async fn pair_finish(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  input: PairInput,
) -> Result<Value, String> {
  let code = PairCode::decode(&Zeroizing::new(input.pairing_code))?;
  let mut pending = host.key_sync.pending.lock().await;
  if !pending.as_ref().is_some_and(|pending| {
    pending.matches(&access.account_key, &code) && pending.deadline > Instant::now()
  }) {
    return Err(EXPIRED.into());
  }
  let path = pair_path(code.id);
  let pair = host.key_sync.pair(access, &path).await?;
  let Some(envelope) = pair.envelope else {
    return Ok(json!(false));
  };
  let context = pair_context(&access.account_key, pair.vault_id, code.id)?;
  let key = crypto::open_vault(&code.secret, &context, &envelope)?;
  verify_account(app, host, &access.account_key).await?;
  save_vault(
    app,
    &access.account_key,
    &LocalVault::new(pair.vault_id, &key),
  )
  .await?;
  *pending = None;
  // The key is already saved. If the receipt is lost, the relay's TTL expires the envelope.
  let _ = host
    .key_sync
    .request(access, Method::DELETE, &path, None)
    .await;
  Ok(json!(true))
}

async fn pair_cancel(app: &AppHandle, host: &Host, input: PairInput) -> Result<Value, String> {
  let code = PairCode::decode(&Zeroizing::new(input.pairing_code))?;
  let Some(account) = host.account.account_key(app).await? else {
    return Ok(Value::Null);
  };
  let mut pending = host.key_sync.pending.lock().await;
  if !pending
    .as_ref()
    .is_some_and(|pending| pending.matches(&account, &code))
  {
    return Ok(Value::Null);
  }
  *pending = None;
  drop(pending);
  let access = host.account.key_exchange_access(app).await?;
  if access.account_key == account {
    let _ = host
      .key_sync
      .request(&access, Method::DELETE, &pair_path(code.id), None)
      .await;
  }
  Ok(Value::Null)
}

async fn pair_approve(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  input: PairInput,
) -> Result<Value, String> {
  let code = PairCode::decode(&Zeroizing::new(input.pairing_code))?;
  let vault = unlocked(app, host, access).await?;
  let path = pair_path(code.id);
  let pair = host.key_sync.pair(access, &path).await?;
  if pair.vault_id != vault.vault_id {
    return Err("This pairing code belongs to a different encryption vault.".into());
  }
  let context = pair_context(&access.account_key, vault.vault_id, code.id)?;
  let secret = vault.key()?;
  if let Some(envelope) = pair.envelope {
    if *crypto::open_vault(&code.secret, &context, &envelope)? == *secret {
      return Ok(Value::Null);
    }
    return Err("This pairing request was already approved with a different key.".into());
  }
  let envelope = crypto::seal(&code.secret, &context, secret.as_ref())?;
  verify_account(app, host, &access.account_key).await?;
  let response = host
    .key_sync
    .request(
      access,
      Method::PUT,
      &format!("{path}/envelope"),
      Some(json!(envelope)),
    )
    .await?;
  acknowledged(response)?;
  Ok(Value::Null)
}

async fn unlocked(app: &AppHandle, host: &Host, access: &Access) -> Result<LocalVault, String> {
  let remote = host.key_sync.vault(access).await?.ok_or(LOCKED)?;
  local_vault(app, &access.account_key)
    .await?
    .filter(|stored| stored.vault_id == remote.vault_id && stored.setup.is_none())
    .ok_or_else(|| LOCKED.into())
}

fn eligible_endpoint(connection: &Connection) -> Option<&'static str> {
  if connection.is_oauth() || matches!(connection.provider_id.as_str(), "custom" | "llama-cpp") {
    return None;
  }
  apell_core::providers::default_base_url(&connection.provider_id)
    .filter(|url| url.starts_with("https://"))
    .map(|url| url.trim_end_matches('/'))
    .filter(|endpoint| connection.base_url.trim_end_matches('/') == *endpoint)
}

fn key_context(account: &str, vault: Uuid, connection: &Connection) -> Result<Vec<u8>, String> {
  let endpoint = eligible_endpoint(connection)
    .ok_or("Only static API keys for built-in provider endpoints can sync.")?;
  crypto::context(
    "api-key",
    account,
    vault,
    &[&connection.connection_id, &connection.provider_id, endpoint],
  )
}

async fn upload(
  app: &AppHandle,
  host: &Host,
  relay: &Access,
  input: UploadInput,
) -> Result<Value, String> {
  if input.connection_ids.is_empty() || input.connection_ids.len() > MAX_CONNECTIONS {
    return Err("Choose one or more saved provider connections.".into());
  }
  let (vault, access, head) = cloud_context(app, host, relay).await?;
  let secret = vault.key()?;
  let expected = head.as_ref().map(|head| head.revision_id);
  let mut entries: BTreeMap<_, _> = head
    .into_iter()
    .flat_map(|head| head.entries)
    .map(|entry| (entry.connection_id.clone(), entry))
    .collect();
  let account = access.account_key.clone();
  let root_app = app.clone();
  let (updated, entries) = {
    let _gate = host.credential_gate.lock().await;
    let settings = host.settings.lock().await.clone();
    blocking(move || {
      let mut updated = 0;
      for id in input.connection_ids {
        let connection = settings.connection(&id)?;
        let context = key_context(&account, vault.vault_id, connection)?;
        let api_key = read_account(&root_app, connection)?
          .and_then(|credentials| credentials.api_key)
          .map(|key| Zeroizing::new(key.into_inner()))
          .filter(|key| !key.is_empty() && key.len() <= MAX_API_KEY)
          .ok_or("A selected connection does not have a supported static API key.")?;
        if let Some(entry) = entries.get(&id) {
          let remote = crypto::open(&secret, &context, &entry.sealed(), MAX_API_KEY)?;
          if remote.as_slice() == api_key.as_bytes() {
            continue;
          }
          if !input.replace_existing {
            return Err("A selected connection already has a different cloud key. Confirm replacement to update it.".into());
          }
        }
        let Sealed { nonce, ciphertext } = crypto::seal(&secret, &context, api_key.as_bytes())?;
        entries.insert(id.clone(), Entry { connection_id: id, nonce, ciphertext });
        updated += 1;
      }
      if entries.len() > MAX_CONNECTIONS {
        return Err("The encrypted API key limit was reached.".into());
      }
      Ok((updated, entries.into_values().collect::<Vec<_>>()))
    }).await?
  };
  if updated > 0 {
    save_head(app, host, &access, vault.vault_id, expected, entries).await?;
  }
  Ok(json!({ "updated": updated }))
}

async fn cloud_context(
  app: &AppHandle,
  host: &Host,
  relay: &Access,
) -> Result<(LocalVault, Access, Option<CloudHead>), String> {
  let vault = unlocked(app, host, relay).await?;
  let access = host.account.access(app).await?;
  if access.account_key != relay.account_key {
    return Err("The signed-in Account changed. Try again.".into());
  }
  let head = host.key_sync.head(&access, vault.vault_id).await?;
  Ok((vault, access, head))
}

async fn save_head(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  vault_id: Uuid,
  expected: Option<Uuid>,
  entries: Vec<Entry>,
) -> Result<(), String> {
  verify_account(app, host, &access.account_key).await?;
  let revision_id = Uuid::new_v4();
  let response = host
    .key_sync
    .request(
      access,
      Method::PUT,
      CLOUD_KEYS,
      Some(json!({
        "expected_head": expected,
        "revision_id": revision_id,
        "vault_id": vault_id,
        "entries": entries,
      })),
    )
    .await?;
  let saved = decode::<HeadReply>(response)
    .await?
    .head
    .ok_or("The Store did not confirm the encrypted update.")?;
  if saved.revision_id != revision_id || saved.vault_id != vault_id || saved.entries != entries {
    return Err("The Store did not confirm the encrypted update.".into());
  }
  Ok(())
}

async fn restore(app: &AppHandle, host: &Host, relay: &Access) -> Result<Value, String> {
  let (vault, access, head) = cloud_context(app, host, relay).await?;
  let Some(head) = head else {
    return Ok(json!({ "updated": 0 }));
  };
  let secret = vault.key()?;
  let _gate = host.credential_gate.lock().await;
  let settings = host.settings.lock().await.clone();
  verify_account(app, host, &access.account_key).await?;
  let app = app.clone();
  let root = host.root.clone();
  let updated = blocking(move || {
    let mut updates = Vec::new();
    for entry in head.entries {
      let Ok(connection) = settings.connection(&entry.connection_id) else {
        continue;
      };
      if eligible_endpoint(connection).is_none() || read_account(&app, connection)?.is_some() {
        continue;
      }
      let context = key_context(&access.account_key, vault.vault_id, connection)?;
      let data = crypto::open(&secret, &context, &entry.sealed(), MAX_API_KEY)?;
      let value =
        std::str::from_utf8(&data).map_err(|_| "An encrypted API key has an invalid format.")?;
      let credential = match connection.credential.format {
        CredentialFormat::ApiKey => value.to_owned(),
        CredentialFormat::Account => encode_account(&apell_core::providers::static_account(
          &connection.provider_id,
          &connection.connection_id,
          Some(value),
        )?)?,
      };
      updates.push((connection.credential.account.clone(), Some(credential)));
    }
    let updated = updates.len();
    if updated > 0 {
      commit_settings(&app, &root, &settings, updates)?;
    }
    Ok(updated)
  })
  .await?;
  Ok(json!({ "updated": updated }))
}

async fn list(app: &AppHandle, host: &Host, relay: &Access) -> Result<Value, String> {
  let (_, access, head) = cloud_context(app, host, relay).await?;
  verify_account(app, host, &access.account_key).await?;
  let Some(head) = head else {
    return Ok(json!({ "revision_id": null, "entries": [] }));
  };
  let settings = host.settings.lock().await;
  let entries: Vec<_> = head
    .entries
    .iter()
    .map(|entry| {
      let name = settings
        .connection(&entry.connection_id)
        .ok()
        .map(|local| &local.name);
      json!({ "connection_id": entry.connection_id, "name": name })
    })
    .collect();
  Ok(json!({ "revision_id": head.revision_id, "entries": entries }))
}

async fn remove(
  app: &AppHandle,
  host: &Host,
  relay: &Access,
  input: RemoveInput,
) -> Result<Value, String> {
  let ids: HashSet<_> = input.connection_ids.iter().collect();
  if ids.is_empty() || ids.len() > MAX_CONNECTIONS || ids.len() != input.connection_ids.len() {
    return Err("Choose one or more cloud key copies to remove.".into());
  }
  let (vault, access, head) = cloud_context(app, host, relay).await?;
  let error = "The cloud key list changed. Review it again before removing copies.";
  let mut head = head
    .filter(|head| head.revision_id == input.expected_head)
    .ok_or(error)?;
  let before = head.entries.len();
  head
    .entries
    .retain(|entry| !ids.contains(&entry.connection_id));
  if before - head.entries.len() != ids.len() {
    return Err(error.into());
  }
  save_head(
    app,
    host,
    &access,
    vault.vault_id,
    Some(head.revision_id),
    head.entries,
  )
  .await?;
  Ok(json!({ "updated": ids.len() }))
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, String> {
  serde_json::from_value(params).map_err(|_| "Invalid encrypted API key sync request.".into())
}

pub(crate) async fn dispatch(app: AppHandle, method: &str, params: Value) -> Result<Value, String> {
  let method = method.to_owned();
  finish_snapshot(async move {
    let host = app.state::<Host>();
    let _gate = host.key_sync.gate.lock().await;
    if method == "key_sync.status" {
      return status(&app, &host).await;
    }
    if method == "key_sync.pair_cancel" {
      return pair_cancel(&app, &host, parse(params)?).await;
    }
    let relay = host.account.key_exchange_access(&app).await?;
    match method.as_str() {
      "key_sync.create" => create(&app, &host, &relay).await,
      "key_sync.confirm_recovery" => confirm_recovery(&app, &host, &relay).await,
      "key_sync.recover" => recover(&app, &host, &relay, parse(params)?).await,
      "key_sync.pair_start" => pair_start(&host, &relay).await,
      "key_sync.pair_finish" => pair_finish(&app, &host, &relay, parse(params)?).await,
      "key_sync.pair_approve" => pair_approve(&app, &host, &relay, parse(params)?).await,
      "key_sync.upload" => upload(&app, &host, &relay, parse(params)?).await,
      "key_sync.restore" => restore(&app, &host, &relay).await,
      "key_sync.list" => list(&app, &host, &relay).await,
      "key_sync.remove" => remove(&app, &host, &relay, parse(params)?).await,
      _ => Err("Unknown encrypted API key sync operation.".into()),
    }
  })
  .await
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::settings::credential_ref;

  #[test]
  fn paired_and_recovered_devices_can_read_the_same_store_entry() {
    let account = "fixture-account";
    let vault_id = Uuid::new_v4();
    let original = crypto::random().unwrap();
    let local = LocalVault::new(vault_id, &original);
    let connection = Connection {
      connection_id: "fixture-connection".into(),
      name: "Fixture connection".into(),
      provider_id: "openai".into(),
      base_url: apell_core::providers::default_base_url("openai")
        .unwrap()
        .into(),
      model_id: "fixture".into(),
      credential: credential_ref("fixture-connection"),
    };
    let aad = key_context(account, vault_id, &connection).unwrap();
    let api_key = b"fixture-key-only-on-the-device";
    let stored = crypto::seal(&local.key().unwrap(), &aad, api_key).unwrap();
    let wire = serde_json::to_string(&stored).unwrap();
    assert!(!wire.contains(std::str::from_utf8(api_key).unwrap()));
    let stored: Sealed = serde_json::from_str(&wire).unwrap();

    let new_device = PairCode::new().unwrap();
    let approved = PairCode::decode(&new_device.encode()).unwrap();
    let pairing = pair_context(account, vault_id, approved.id).unwrap();
    let envelope = crypto::seal(&approved.secret, &pairing, original.as_ref()).unwrap();
    let paired_key = crypto::open_vault(&new_device.secret, &pairing, &envelope).unwrap();

    let offline = crypto::random().unwrap();
    let recovery = crypto::context("recovery", account, vault_id, &[]).unwrap();
    let wrapped = crypto::seal(&offline, &recovery, original.as_ref()).unwrap();
    let recovered_key = crypto::open_vault(&offline, &recovery, &wrapped).unwrap();
    for key in [paired_key, recovered_key] {
      assert_eq!(
        crypto::open(&key, &aad, &stored, MAX_API_KEY)
          .unwrap()
          .as_slice(),
        api_key
      );
    }

    let moved = Connection {
      base_url: "https://other.example.invalid".into(),
      ..connection
    };
    assert!(key_context(account, vault_id, &moved).is_err());
  }
}

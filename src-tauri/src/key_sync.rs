mod automatic;
mod crypto;
mod managed;

pub(crate) use automatic::{commit_local_credentials, kick, start};

use crate::{
  account::Access,
  commands::blocking,
  host::Host,
  provider_commands::finish_snapshot,
  settings::{Connection, MAX_CONNECTIONS},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use crypto::{Sealed, Secret};
use reqwest::{Client, Method, Response, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{future::Future, time::Duration};
use tauri::{AppHandle, Manager};
use tokio::sync::{watch, Mutex};
use uuid::Uuid;
use zeroize::{Zeroize, Zeroizing};

const CLOUD_KEYS: &str = "/api/v1/credential-sync/head";
const MAX_REPLY: usize = 192 * 1024;
const LOCKED: &str = "Verify this device to sync encrypted API keys.";
const SIGNING_OUT: &str = "Encrypted API key sync stopped for sign-out.";

pub(crate) struct KeySync {
  client: Client,
  pub gate: Mutex<()>,
  sign_out: Mutex<()>,
  signing_out: watch::Sender<bool>,
  managed: managed::Managed,
  automatic: automatic::Automatic,
}

pub(crate) struct BackgroundStop(watch::Receiver<bool>);

impl BackgroundStop {
  fn requested(&self) -> bool {
    *self.0.borrow()
  }

  pub(crate) fn check(&self) -> Result<(), String> {
    if self.requested() {
      Err(SIGNING_OUT.into())
    } else {
      Ok(())
    }
  }

  pub(crate) async fn wait<T>(&self, work: impl Future<Output = T>) -> Result<T, String> {
    let mut requested = self.0.clone();
    tokio::select! {
      biased;
      _ = requested.wait_for(|requested| *requested) => Err(SIGNING_OUT.into()),
      result = work => Ok(result),
    }
  }

  async fn access(
    &self,
    app: &AppHandle,
    host: &Host,
    key_exchange: bool,
    same_session_as: Option<&Access>,
  ) -> Result<Access, String> {
    let task = self
      .wait(
        host
          .account
          .start_background_access(app, key_exchange, same_session_as),
      )
      .await??;
    self
      .wait(task)
      .await?
      .map_err(|_| "Could not refresh Apell Account access.")?
  }
}

struct PauseBackground<'a>(&'a watch::Sender<bool>);

impl<'a> PauseBackground<'a> {
  fn new(signal: &'a watch::Sender<bool>) -> Self {
    signal.send_replace(true);
    Self(signal)
  }
}

impl Drop for PauseBackground<'_> {
  fn drop(&mut self) {
    self.0.send_replace(false);
  }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LocalVault {
  vault_id: Uuid,
  key: String,
  setup: Option<PendingSetup>,
  #[serde(default)]
  managed: bool,
  #[serde(default)]
  pending_managed: bool,
}

impl LocalVault {
  fn new(vault_id: Uuid, key: &Secret) -> Self {
    Self {
      vault_id,
      key: URL_SAFE_NO_PAD.encode(key.as_ref()),
      setup: None,
      managed: false,
      pending_managed: false,
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

impl KeySync {
  pub fn new(client: Client) -> Self {
    Self {
      client,
      gate: Mutex::new(()),
      sign_out: Mutex::new(()),
      signing_out: watch::channel(false).0,
      managed: managed::Managed::default(),
      automatic: automatic::Automatic::default(),
    }
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

async fn decode<T: DeserializeOwned>(response: Response) -> Result<T, String> {
  if response.status() != StatusCode::OK {
    return Err(response_error(response.status()));
  }
  decode_body(response).await
}

async fn decode_body<T: DeserializeOwned>(mut response: Response) -> Result<T, String> {
  let mut body = Zeroizing::new(Vec::new());
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
    StatusCode::NOT_FOUND | StatusCode::GONE => "This request expired. Start again.".into(),
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

async fn unlocked(app: &AppHandle, host: &Host, relay: &Access) -> Result<LocalVault, String> {
  let local = local_vault(app, &relay.account_key).await?.ok_or(LOCKED)?;
  if !local.managed || local.setup.is_some() || local.pending_managed {
    return Err(LOCKED.into());
  }
  let remote = managed::remote(host, relay).await?;
  if !remote.managed || remote.vault_id != Some(local.vault_id) {
    return Err(LOCKED.into());
  }
  Ok(local)
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

async fn cloud_context(
  app: &AppHandle,
  host: &Host,
  relay: &Access,
  stop: &BackgroundStop,
) -> Result<(LocalVault, Access, Option<CloudHead>), String> {
  let vault = stop.wait(unlocked(app, host, relay)).await??;
  let access = stop.access(app, host, false, Some(relay)).await?;
  let head = stop
    .wait(host.key_sync.head(&access, vault.vault_id))
    .await??;
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
  automatic::require_device_sync(&host.root).await?;
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

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, String> {
  serde_json::from_value(params).map_err(|_| "Invalid encrypted API key sync request.".into())
}

pub(crate) async fn dispatch(app: AppHandle, method: &str, params: Value) -> Result<Value, String> {
  let method = method.to_owned();
  finish_snapshot(async move {
    let host = app.state::<Host>();
    match method.as_str() {
      "key_sync.status" => return managed::status(&app, &host).await,
      "key_sync.retry" => return managed::retry(&app, &host).await,
      _ => {}
    }
    let _gate = host.key_sync.gate.lock().await;
    let relay = host.account.key_exchange_access(&app).await?;
    match method.as_str() {
      "key_sync.create" => managed::create(&app, &host, &relay).await,
      "key_sync.pair_start" => managed::start_pair(&host, &relay).await,
      "key_sync.pair_list" => managed::list_pairs(&app, &host, &relay).await,
      "key_sync.pair_finish" => managed::finish_pair(&app, &host, &relay, parse(params)?).await,
      "key_sync.pair_cancel" => managed::cancel_pair(&host, &relay, parse(params)?).await,
      "key_sync.pair_approve" => managed::approve_pair(&app, &host, &relay, parse(params)?).await,
      "key_sync.recovery_start" => managed::start_recovery(&app, &host, &relay).await,
      "key_sync.recovery_open" => managed::open_recovery(&app, &host, &relay).await,
      "key_sync.recovery_finish" => managed::finish_recovery(&app, &host, &relay).await,
      "key_sync.recovery_cancel" => managed::cancel_recovery(&host).await,
      _ => Err("Unknown encrypted API key sync operation.".into()),
    }
  })
  .await
}

pub(crate) async fn sign_out(app: AppHandle) -> Result<Value, String> {
  finish_snapshot(async move {
    let host = app.state::<Host>();
    let sync = &host.key_sync;
    let _sign_out = sync.sign_out.lock().await;
    let pause = PauseBackground::new(&sync.signing_out);
    let _gate = sync.gate.lock().await;
    let result = crate::account::dispatch(app.clone(), &host.account, "account.sign_out").await;
    if result.is_ok() {
      sync.managed.clear().await;
      sync.automatic.publish_health(&app, None).await;
    }
    // Resume the worker before releasing its gate, including on a failed sign-out.
    drop(pause);
    if result.is_err() {
      kick(&app);
    }
    result
  })
  .await
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::settings::credential_ref;
  use crypto::{PairCode, MAX_API_KEY};
  use tokio::sync::oneshot;

  #[tokio::test]
  async fn pending_sign_out_releases_background_wait_and_keeps_refresh_running() {
    let sync = std::sync::Arc::new(KeySync::new(Client::new()));
    let (entered, entered_wait) = oneshot::channel();
    let (reply, network_reply) = oneshot::channel();
    let (finished, refresh_finished) = oneshot::channel();
    let background = sync.clone();
    let worker = tokio::spawn(async move {
      let _gate = background.gate.lock().await;
      let stop = BackgroundStop(background.signing_out.subscribe());
      let refresh = tokio::spawn(async move {
        network_reply.await.unwrap();
        finished.send(()).unwrap();
      });
      entered.send(()).unwrap();
      stop.wait(refresh).await
    });
    entered_wait.await.unwrap();

    let pause = PauseBackground::new(&sync.signing_out);
    let _gate = tokio::time::timeout(Duration::from_secs(1), sync.gate.lock())
      .await
      .expect("Sign-out must not wait for the background network request");
    assert_eq!(worker.await.unwrap().unwrap_err(), SIGNING_OUT);
    let pending = BackgroundStop(sync.signing_out.subscribe());
    assert!(pending.check().is_err());
    assert!(pending
      .wait(async { panic!("No new work should start during sign-out") })
      .await
      .is_err());
    reply.send(()).unwrap();
    refresh_finished.await.unwrap();
    drop(pause);
    assert!(pending.check().is_ok());
  }

  #[tokio::test]
  async fn sign_out_serializes_with_an_already_started_local_commit() {
    let sync = std::sync::Arc::new(KeySync::new(Client::new()));
    let (started, commit_started) = oneshot::channel();
    let (release, finish_commit) = oneshot::channel();
    let background = sync.clone();
    let worker = tokio::spawn(async move {
      let _gate = background.gate.lock().await;
      let stop = BackgroundStop(background.signing_out.subscribe());
      stop.check().unwrap();
      started.send(()).unwrap();
      finish_commit.await.unwrap();
      stop.check()
    });
    commit_started.await.unwrap();

    let _pause = PauseBackground::new(&sync.signing_out);
    assert!(sync.gate.try_lock().is_err());
    release.send(()).unwrap();
    assert_eq!(worker.await.unwrap().unwrap_err(), SIGNING_OUT);
    assert!(sync.gate.try_lock().is_ok());
  }

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
    let pairing = crypto::context("pair", account, vault_id, &[&approved.id.to_string()]).unwrap();
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

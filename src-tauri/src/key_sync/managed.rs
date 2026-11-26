use super::{
  acknowledged, automatic, automatic::SyncHealth, crypto, decode, decode_body, kick, local_vault,
  response_error, save_vault, unlocked, verify_account, Access, AppHandle, BackgroundStop, Host,
  LocalVault, Method, Secret, LOCKED,
};
use apell_vault_keys::derive_pairing_proof_key;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use reqwest::{Response, StatusCode};
use ring::hmac;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{future::Future, sync::Arc};
use tauri::{Manager, Url};
use tauri_plugin_opener::OpenerExt;
use time::OffsetDateTime;
use tokio::sync::{watch, Mutex, MutexGuard};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::settings::Settings;

const MANAGED: &str = "/api/v1/key-vault/managed";
const EXPIRED: &str = "This request expired. Start again.";

#[derive(Default)]
pub(super) struct Managed {
  joining: Mutex<Option<Arc<Joining>>>,
  recovery: Mutex<Option<Arc<Recovery>>>,
}

impl Managed {
  pub async fn clear(&self) {
    if let Some(request) = self.joining.lock().await.take() {
      request.cancel.signal();
    }
    if let Some(request) = self.recovery.lock().await.take() {
      request.cancel.signal();
    }
  }
}

#[derive(Clone)]
pub(super) struct Cancellation(watch::Sender<bool>);

impl Cancellation {
  fn new() -> Self {
    Self(watch::channel(false).0)
  }

  fn signal(&self) {
    self.0.send_replace(true);
  }

  fn check(&self) -> Result<(), String> {
    if *self.0.borrow() {
      Err(EXPIRED.into())
    } else {
      Ok(())
    }
  }

  pub(super) async fn wait<T>(
    &self,
    stop: &BackgroundStop,
    work: impl Future<Output = Result<T, String>>,
  ) -> Result<T, String> {
    let mut cancelled = self.0.subscribe();
    stop
      .wait(async {
        tokio::select! {
          biased;
          _ = cancelled.wait_for(|cancelled| *cancelled) => Err(EXPIRED.into()),
          result = work => result,
        }
      })
      .await?
  }
}

struct Joining {
  access: Access,
  vault_id: Uuid,
  pair: Pair,
  secret: Secret,
  cancel: Cancellation,
}

struct Recovery {
  account: String,
  request_id: Uuid,
  vault_id: Uuid,
  challenge: Challenge,
  secret: Secret,
  enrolling: bool,
  cancel: Cancellation,
}

#[derive(Clone, Copy, Deserialize)]
pub(super) struct RemoteVault {
  pub vault_id: Option<Uuid>,
  pub managed: bool,
}

#[derive(Deserialize, Serialize)]
struct Pair {
  pair_id: Uuid,
  code: String,
  expires_at: i64,
  approved: bool,
  approval_nonce: String,
}

impl Pair {
  fn valid(&self, now: i64) -> bool {
    self.pair_id.get_version_num() == 4
      && self.code.len() == 6
      && self.code.bytes().all(|byte| byte.is_ascii_digit())
      && valid_approval_nonce(&self.approval_nonce)
      && self.expires_at > now
  }
}

#[derive(Deserialize)]
pub(super) struct PairInput {
  pair_id: Uuid,
}

#[derive(Deserialize)]
pub(super) struct ApproveInput {
  pair_id: Uuid,
  approval_nonce: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecoveryInput {
  request_id: Uuid,
}

#[derive(Deserialize)]
struct PairList {
  pairs: Vec<Pair>,
}

#[derive(Deserialize)]
struct PairResult {
  vault_key: Option<String>,
}

#[derive(Deserialize)]
struct Challenge {
  recovery_id: String,
  authorization_url: Url,
  expires_at: i64,
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum RecoveryResult {
  Pending,
  Verified,
  Complete {
    vault_key: Option<String>,
  },
  #[serde(skip)]
  Rejected,
}

pub(super) async fn remote(host: &Host, access: &Access) -> Result<RemoteVault, String> {
  let response = host
    .key_sync
    .request(access, Method::GET, MANAGED, None)
    .await?;
  decode_remote(response).await
}

async fn decode_remote(response: Response) -> Result<RemoteVault, String> {
  if matches!(
    response.status(),
    StatusCode::NOT_FOUND | StatusCode::GONE | StatusCode::SERVICE_UNAVAILABLE
  ) {
    return Err("Device verification is temporarily unavailable. Try again later.".into());
  }
  decode(response).await
}

pub(super) fn is_verified(local: &LocalVault) -> bool {
  local.managed && !local.pending_managed && local.setup.is_none()
}

pub(super) async fn status(
  app: &AppHandle,
  host: &Host,
  local_only: bool,
) -> Result<Value, String> {
  if !host.account.key_exchange_available() {
    return Ok(json!({ "state": "unavailable" }));
  }
  let stop = BackgroundStop(host.key_sync.signing_out.subscribe());
  let Some(account) = stop.wait(host.account.account_key(app)).await?? else {
    return Ok(json!({ "state": "signed_out" }));
  };
  let local = stop.wait(local_vault(app, &account)).await??;
  if local.as_ref().is_some_and(is_verified) {
    let health = match automatic::require_device_sync(&host.root).await {
      Ok(()) => host.key_sync.automatic.health(&account).await,
      Err(error) => SyncHealth::Paused(error),
    };
    stop.wait(verify_account(app, host, &account)).await??;
    return Ok(health.verified_status());
  }
  if local_only {
    stop.wait(verify_account(app, host, &account)).await??;
    return Ok(json!({ "state": "unverified" }));
  }
  let discovery = async {
    let access = stop.access(app, host, true, None).await?;
    if access.account_key != account {
      return Err("The signed-in Account changed. Try again.".into());
    }
    stop.wait(remote(host, &access)).await?
  }
  .await;
  stop.wait(verify_account(app, host, &account)).await??;
  Ok(unverified_status(local.as_ref(), discovery))
}

fn unverified_status(local: Option<&LocalVault>, remote: Result<RemoteVault, String>) -> Value {
  let remote = match remote {
    Ok(remote) => remote,
    Err(error) => return json!({ "state": "unverified", "action": "check", "error": error }),
  };
  let local_matches = local.is_some_and(|value| Some(value.vault_id) == remote.vault_id);
  if remote.vault_id.is_none() || (remote.managed && local_matches) {
    json!({ "state": "unverified", "action": "setup" })
  } else if remote.managed {
    json!({ "state": "unverified", "action": "pair" })
  } else if local_matches {
    json!({ "state": "unverified", "action": "upgrade" })
  } else {
    json!({ "state": "unverified", "action": "legacy" })
  }
}

pub(super) async fn retry(app: &AppHandle, host: &Host) -> Result<Value, String> {
  let stop = BackgroundStop(host.key_sync.signing_out.subscribe());
  stop.check()?;
  if !host.account.key_exchange_available() {
    return Err(LOCKED.into());
  }
  let account = host.account.account_key(app).await?.ok_or(LOCKED)?;
  let vault = local_vault(app, &account)
    .await?
    .filter(is_verified)
    .ok_or(LOCKED)?;
  vault.key()?;
  automatic::require_device_sync(&host.root).await?;
  verify_account(app, host, &account).await?;
  host
    .key_sync
    .automatic
    .queue_retry(app, account, &stop)
    .await?;
  Ok(Value::Null)
}

async fn prepare_activation<'a>(
  app: &AppHandle,
  host: &'a Host,
  access: &Access,
  stop: &BackgroundStop,
) -> Result<(MutexGuard<'a, ()>, Settings), String> {
  let credential = stop.wait(host.credential_gate.lock()).await?;
  let settings = stop.wait(host.settings.lock()).await?.clone();
  stop
    .wait(verify_account(app, host, &access.account_key))
    .await??;
  Ok((credential, settings))
}

// The caller holds the sync and credential gates until the local writes finish.
async fn activate(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  mut vault: LocalVault,
  settings: Settings,
) -> Result<(), String> {
  vault.managed = true;
  vault.pending_managed = true;
  vault.setup = None;
  save_vault(app, &access.account_key, &vault).await?;
  automatic::activate(app, host, &access.account_key, vault.vault_id, settings).await?;
  vault.pending_managed = false;
  save_vault(app, &access.account_key, &vault).await?;
  kick(app);
  Ok(())
}

pub(super) async fn create(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  stop: &BackgroundStop,
) -> Result<Value, String> {
  let remote = stop.wait(remote(host, access)).await??;
  let saved = stop.wait(local_vault(app, &access.account_key)).await??;
  let mut vault = match (remote.vault_id, saved) {
    (Some(id), Some(saved)) if remote.managed && saved.vault_id == id => saved,
    (Some(_), _) => return Err("Pair this device with the existing encryption vault.".into()),
    (None, Some(saved)) if saved.pending_managed || saved.setup.is_some() => saved,
    (None, Some(_)) => return Err("The saved vault is missing from Account.".into()),
    (None, None) => LocalVault::new(Uuid::new_v4(), &crypto::random()?),
  };
  vault.pending_managed = true;
  stop
    .wait(verify_account(app, host, &access.account_key))
    .await??;
  stop.check()?;
  save_vault(app, &access.account_key, &vault).await?;
  let response = stop
    .wait(host.key_sync.request(
      access,
      Method::PUT,
      MANAGED,
      Some(json!({
        "vault_id": vault.vault_id, "vault_key": encode(&vault.key()?).as_str(),
      })),
    ))
    .await??;
  acknowledged(response)?;
  let (_credential, settings) = prepare_activation(app, host, access, stop).await?;
  stop.check()?;
  activate(app, host, access, vault, settings).await?;
  Ok(Value::Null)
}

fn encode(secret: &Secret) -> Zeroizing<String> {
  Zeroizing::new(URL_SAFE_NO_PAD.encode(secret.as_ref()))
}

fn valid_approval_nonce(value: &str) -> bool {
  let mut nonce = [0; 16];
  value.len() == 22 && URL_SAFE_NO_PAD.decode_slice(value, &mut nonce) == Ok(nonce.len())
}

fn encode_proof(
  vault: &LocalVault,
  approval: Option<&ApproveInput>,
) -> Result<Zeroizing<String>, String> {
  let vault_id = vault.vault_id.to_string();
  let message = match approval {
    Some(approval) if valid_approval_nonce(&approval.approval_nonce) => format!(
      "apell-key-relay-v2\napprove\n{vault_id}\n{}\n{}",
      approval.pair_id, approval.approval_nonce
    ),
    Some(_) => return Err("Invalid device approval.".into()),
    None => format!("apell-key-relay-v2\nlist\n{vault_id}\n"),
  };
  let vault_key = vault.key()?;
  let proof_key = derive_pairing_proof_key(&vault_key, &vault_id);
  let signing_key = hmac::Key::new(hmac::HMAC_SHA256, proof_key.as_ref());
  let tag = hmac::sign(&signing_key, message.as_bytes());
  Ok(Zeroizing::new(URL_SAFE_NO_PAD.encode(tag.as_ref())))
}

pub(super) async fn start_pair(
  host: &Host,
  access: Access,
  stop: &BackgroundStop,
) -> Result<Value, String> {
  let vault = stop.wait(remote(host, &access)).await??;
  let vault_id = vault.vault_id.filter(|_| vault.managed).ok_or(LOCKED)?;
  let pair_id = Uuid::new_v4();
  let secret = crypto::random()?;
  let response = stop
    .wait(host.key_sync.request(
      &access,
      Method::PUT,
      &format!("{MANAGED}/pairs/{pair_id}"),
      Some(json!({
        "vault_id": vault_id, "request_secret": encode(&secret).as_str(),
      })),
    ))
    .await??;
  let pair: Pair = stop.wait(decode(response)).await??;
  if pair.pair_id != pair_id || pair.approved || !pair.valid(now()) {
    return Err("Account returned an invalid pairing request.".into());
  }
  let display = serde_json::to_value(&pair).map_err(|_| "Could not show the pairing code.")?;
  let mut pending = stop.wait(host.key_sync.managed.joining.lock()).await?;
  stop.check()?;
  if let Some(previous) = pending.replace(Arc::new(Joining {
    access,
    vault_id,
    pair,
    secret,
    cancel: Cancellation::new(),
  })) {
    previous.cancel.signal();
  }
  Ok(display)
}

pub(super) async fn list_pairs(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  stop: &BackgroundStop,
) -> Result<Value, String> {
  let vault = stop.wait(unlocked(app, host, access)).await??;
  let proof = encode_proof(&vault, None)?;
  let response = stop
    .wait(host.key_sync.request(
      access,
      Method::POST,
      &format!("{MANAGED}/pairs/list"),
      Some(json!({ "vault_id": vault.vault_id, "proof": proof.as_str() })),
    ))
    .await??;
  let pairs: PairList = stop.wait(decode(response)).await??;
  if pairs.pairs.len() > 16 || pairs.pairs.iter().any(|pair| !pair.valid(now())) {
    return Err("Account returned an invalid pairing list.".into());
  }
  Ok(json!({ "pairs": pairs.pairs }))
}

pub(super) async fn approve_pair(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  input: ApproveInput,
  stop: &BackgroundStop,
) -> Result<Value, String> {
  let vault = stop.wait(unlocked(app, host, access)).await??;
  let proof = encode_proof(&vault, Some(&input))?;
  let response = stop
    .wait(host.key_sync.request(
      access,
      Method::POST,
      &format!("{MANAGED}/pairs/{}/approve", input.pair_id),
      Some(json!({ "vault_id": vault.vault_id, "proof": proof.as_str() })),
    ))
    .await??;
  acknowledged(response)?;
  Ok(Value::Null)
}

fn joining<'a>(
  stored: &'a Option<Arc<Joining>>,
  account: &str,
  pair_id: Uuid,
) -> Result<&'a Arc<Joining>, String> {
  stored
    .as_ref()
    .filter(|pending| {
      pending.access.account_key == account
        && pending.pair.pair_id == pair_id
        && pending.pair.valid(now())
    })
    .ok_or_else(|| EXPIRED.into())
}

pub(super) async fn pair_cancellation(
  host: &Host,
  account: &str,
  input: &PairInput,
  stop: &BackgroundStop,
) -> Result<Cancellation, String> {
  let pending = stop.wait(host.key_sync.managed.joining.lock()).await?;
  stop.check()?;
  Ok(joining(&pending, account, input.pair_id)?.cancel.clone())
}

fn require_current_request<T>(
  pending: &Option<Arc<T>>,
  request: &Arc<T>,
  stop: &BackgroundStop,
) -> Result<(), String> {
  stop.check()?;
  if pending
    .as_ref()
    .is_some_and(|current| Arc::ptr_eq(current, request))
  {
    Ok(())
  } else {
    Err(EXPIRED.into())
  }
}

fn consume_pair(app: &AppHandle, request: Arc<Joining>, stop: &BackgroundStop) {
  let app = app.clone();
  let stop = stop.clone();
  tauri::async_runtime::spawn(async move {
    let host = app.state::<Host>();
    // The original Access is session-bound. Cleanup must not refresh it or
    // use a newly signed-in session, and abandoned pairs expire at the relay.
    let _ = stop
      .wait(host.key_sync.request(
        &request.access,
        Method::POST,
        &format!("{MANAGED}/pairs/{}/consume", request.pair.pair_id),
        Some(json!({ "request_secret": encode(&request.secret).as_str() })),
      ))
      .await;
  });
}

pub(super) async fn finish_pair(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  input: PairInput,
  stop: &BackgroundStop,
) -> Result<Value, String> {
  let request = {
    let pending = stop.wait(host.key_sync.managed.joining.lock()).await?;
    joining(&pending, &access.account_key, input.pair_id)?.clone()
  };
  let result: PairResult = request
    .cancel
    .wait(stop, async {
      let response = host
        .key_sync
        .request(
          access,
          Method::POST,
          &format!("{MANAGED}/pairs/{}/finish", input.pair_id),
          Some(json!({ "request_secret": encode(&request.secret).as_str() })),
        )
        .await?;
      decode(response).await
    })
    .await?;
  let Some(key) = result.vault_key.map(Zeroizing::new) else {
    return Ok(json!(false));
  };
  let key = crypto::decode_key(&key).ok_or("Account returned an invalid vault key.")?;
  let vault = LocalVault::new(request.vault_id, &key);
  let (_credential, settings) = request
    .cancel
    .wait(stop, prepare_activation(app, host, access, stop))
    .await?;
  {
    let mut pending = stop.wait(host.key_sync.managed.joining.lock()).await?;
    require_current_request(&pending, &request, stop)?;
    activate(app, host, access, vault, settings).await?;
    *pending = None;
  }
  consume_pair(app, request, stop);
  Ok(json!(true))
}

pub(super) async fn cancel_pair(
  app: &AppHandle,
  host: &Host,
  input: PairInput,
  stop: &BackgroundStop,
) -> Result<Value, String> {
  let Some(account) = stop.wait(host.account.account_key(app)).await?? else {
    return Ok(Value::Null);
  };
  let request = {
    let mut pending = stop.wait(host.key_sync.managed.joining.lock()).await?;
    stop.check()?;
    pending.take_if(|request| {
      request.access.account_key == account && request.pair.pair_id == input.pair_id
    })
  };
  if let Some(request) = request {
    request.cancel.signal();
    consume_pair(app, request, stop);
  }
  Ok(Value::Null)
}

pub(super) async fn start_recovery(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  stop: &BackgroundStop,
) -> Result<Value, String> {
  let remote = stop.wait(remote(host, access)).await??;
  let vault_id = remote.vault_id.ok_or(LOCKED)?;
  let enrolling = !remote.managed;
  if enrolling
    && !stop
      .wait(local_vault(app, &access.account_key))
      .await??
      .is_some_and(|vault| vault.vault_id == vault_id)
  {
    return Err("Open Apell on a device with this vault to finish its recovery setup.".into());
  }
  let secret = crypto::random()?;
  let request_secret_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_ref()));
  let response = stop.wait(host.key_sync.request(access, Method::POST, &format!("{MANAGED}/recovery/start"), Some(json!({
    "vault_id": vault_id, "purpose": if enrolling { "enroll" } else { "recover" }, "request_secret_hash": request_secret_hash,
  })))).await??;
  let challenge: Challenge = stop.wait(decode(response)).await??;
  if crypto::decode_key(&challenge.recovery_id).is_none()
    || challenge.expires_at <= now()
    || !valid_authorization_url(&challenge.authorization_url, &access.sync_url)
  {
    return Err("Account returned an invalid verification request.".into());
  }
  let request_id = Uuid::new_v4();
  let expires_at = challenge.expires_at;
  let mut pending = stop.wait(host.key_sync.managed.recovery.lock()).await?;
  stop.check()?;
  if let Some(previous) = pending.replace(Arc::new(Recovery {
    account: access.account_key.clone(),
    request_id,
    vault_id,
    challenge,
    secret,
    enrolling,
    cancel: Cancellation::new(),
  })) {
    previous.cancel.signal();
  }
  Ok(json!({ "request_id": request_id, "expires_at": expires_at }))
}

fn valid_authorization_url(url: &Url, account: &Url) -> bool {
  url.scheme() == "https"
    && url.origin() == account.origin()
    && url.path().ends_with("/oidc/auth")
    && url.username().is_empty()
    && url.password().is_none()
    && url.fragment().is_none()
}

fn recovery<'a>(
  stored: &'a Option<Arc<Recovery>>,
  account: &str,
  request_id: Uuid,
) -> Result<&'a Arc<Recovery>, String> {
  stored
    .as_ref()
    .filter(|pending| {
      pending.account == account
        && pending.request_id == request_id
        && pending.challenge.expires_at > now()
    })
    .ok_or_else(|| EXPIRED.into())
}

pub(super) async fn recovery_cancellation(
  host: &Host,
  account: &str,
  input: &RecoveryInput,
  stop: &BackgroundStop,
) -> Result<Cancellation, String> {
  let pending = stop.wait(host.key_sync.managed.recovery.lock()).await?;
  stop.check()?;
  Ok(
    recovery(&pending, account, input.request_id)?
      .cancel
      .clone(),
  )
}

pub(super) async fn open_recovery(
  app: &AppHandle,
  host: &Host,
  input: RecoveryInput,
  stop: &BackgroundStop,
) -> Result<Value, String> {
  let account = stop
    .wait(host.account.account_key(app))
    .await??
    .ok_or(EXPIRED)?;
  let (url, cancellation) = {
    let pending = stop.wait(host.key_sync.managed.recovery.lock()).await?;
    let request = recovery(&pending, &account, input.request_id)?;
    (
      request.challenge.authorization_url.clone(),
      request.cancel.clone(),
    )
  };
  stop.check()?;
  cancellation.check()?;
  app
    .opener()
    .open_url(url.as_str(), None::<&str>)
    .map_err(|_| "Could not open Account verification.")?;
  Ok(Value::Null)
}

pub(super) async fn finish_recovery(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  input: RecoveryInput,
  stop: &BackgroundStop,
) -> Result<Value, String> {
  let request = {
    let pending = stop.wait(host.key_sync.managed.recovery.lock()).await?;
    recovery(&pending, &access.account_key, input.request_id)?.clone()
  };
  let path = format!(
    "{MANAGED}/recovery/{}/finish",
    request.challenge.recovery_id
  );
  let proof = encode(&request.secret);
  let result = request
    .cancel
    .wait(stop, async {
      let response = host
        .key_sync
        .request(
          access,
          Method::POST,
          &path,
          Some(json!({ "request_secret": proof.as_str() })),
        )
        .await?;
      decode_recovery(response).await
    })
    .await?;
  let vault = match result {
    RecoveryResult::Pending => return Ok(json!(false)),
    RecoveryResult::Rejected => {
      let mut pending = stop.wait(host.key_sync.managed.recovery.lock()).await?;
      require_current_request(&pending, &request, stop)?;
      *pending = None;
      return Ok(json!("rejected"));
    }
    RecoveryResult::Verified if request.enrolling => {
      request
        .cancel
        .wait(stop, async {
          let vault = local_vault(app, &access.account_key)
            .await?
            .filter(|vault| vault.vault_id == request.vault_id)
            .ok_or(LOCKED)?;
          let key = encode(&vault.key()?);
          let response = host
            .key_sync
            .request(
              access,
              Method::POST,
              &path,
              Some(json!({ "request_secret": proof.as_str(), "vault_key": key.as_str() })),
            )
            .await?;
          if !matches!(
            decode::<RecoveryResult>(response).await?,
            RecoveryResult::Complete { vault_key: None }
          ) {
            return Err("Account did not complete the recovery setup.".into());
          }
          Ok(vault)
        })
        .await?
    }
    RecoveryResult::Complete {
      vault_key: Some(key),
    } if !request.enrolling => {
      let key = Zeroizing::new(key);
      let secret = crypto::decode_key(&key).ok_or("Account returned an invalid vault key.")?;
      LocalVault::new(request.vault_id, &secret)
    }
    _ => return Err("Account did not complete verification.".into()),
  };
  let (_credential, settings) = request
    .cancel
    .wait(stop, prepare_activation(app, host, access, stop))
    .await?;
  let mut pending = stop.wait(host.key_sync.managed.recovery.lock()).await?;
  require_current_request(&pending, &request, stop)?;
  activate(app, host, access, vault, settings).await?;
  *pending = None;
  Ok(json!(true))
}

async fn decode_recovery(response: Response) -> Result<RecoveryResult, String> {
  if response.status() == StatusCode::FORBIDDEN {
    if let Ok(error) = decode_body::<Value>(response).await {
      if error.pointer("/error/code").and_then(Value::as_str)
        == Some("key_recovery_verification_failed")
      {
        return Ok(RecoveryResult::Rejected);
      }
    }
    return Err(response_error(StatusCode::FORBIDDEN));
  }
  decode(response).await
}

pub(super) async fn cancel_recovery(
  app: &AppHandle,
  host: &Host,
  input: RecoveryInput,
  stop: &BackgroundStop,
) -> Result<Value, String> {
  let Some(account) = stop.wait(host.account.account_key(app)).await?? else {
    return Ok(Value::Null);
  };
  let mut pending = stop.wait(host.key_sync.managed.recovery.lock()).await?;
  stop.check()?;
  if let Some(request) =
    pending.take_if(|request| request.account == account && request.request_id == input.request_id)
  {
    request.cancel.signal();
  }
  Ok(Value::Null)
}

fn now() -> i64 {
  OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;

  #[tokio::test]
  async fn cancelling_a_request_releases_its_pending_relay_wait() {
    let (_session, sign_out) = watch::channel(false);
    let stop = BackgroundStop(sign_out);
    let cancel = Arc::new(Cancellation::new());
    let (started, entered) = tokio::sync::oneshot::channel();
    let work = tokio::spawn({
      let cancel = cancel.clone();
      async move {
        cancel
          .wait(&stop, async move {
            started.send(()).unwrap();
            std::future::pending::<Result<(), String>>().await
          })
          .await
      }
    });
    entered.await.unwrap();
    cancel.signal();
    let result = tokio::time::timeout(Duration::from_secs(1), work)
      .await
      .unwrap()
      .unwrap();
    assert_eq!(result.unwrap_err(), EXPIRED);
  }

  #[test]
  fn replacement_cannot_commit_a_former_request_snapshot() {
    let (sign_out, stop) = watch::channel(false);
    let stop = BackgroundStop(stop);
    let request = Arc::new("fixture");
    assert!(require_current_request(&Some(request.clone()), &request, &stop).is_ok());
    assert!(require_current_request(&Some(Arc::new("fixture")), &request, &stop).is_err());
    assert!(require_current_request(&None, &request, &stop).is_err());

    sign_out.send_replace(true);
    assert!(require_current_request(&Some(request.clone()), &request, &stop).is_err());
  }

  #[test]
  fn recovery_lookup_binds_the_local_request_id_to_its_account() {
    let request_id = Uuid::new_v4();
    let stored = Some(Arc::new(Recovery {
      account: "fixture".into(),
      request_id,
      vault_id: Uuid::new_v4(),
      challenge: Challenge {
        recovery_id: "relay-id".into(),
        authorization_url: Url::parse("https://account.example.invalid/oidc/auth").unwrap(),
        expires_at: now() + 60,
      },
      secret: crypto::random().unwrap(),
      enrolling: false,
      cancel: Cancellation::new(),
    }));
    assert!(recovery(&stored, "fixture", request_id).is_ok());
    assert!(recovery(&stored, "another", request_id).is_err());
    assert!(recovery(&stored, "fixture", Uuid::new_v4()).is_err());
  }

  #[tokio::test]
  async fn recovery_rejection_is_distinct_from_a_forbidden_account() {
    let response = |code| {
      tauri::http::Response::builder()
        .status(StatusCode::FORBIDDEN)
        .body(json!({ "error": { "code": code } }).to_string())
        .unwrap()
        .into()
    };
    assert!(matches!(
      decode_recovery(response("key_recovery_verification_failed")).await,
      Ok(RecoveryResult::Rejected)
    ));
    assert_eq!(
      decode_recovery(response("permission_denied")).await.err(),
      Some(response_error(StatusCode::FORBIDDEN))
    );
  }

  #[tokio::test]
  async fn missing_managed_service_reports_unavailability() {
    for status in [
      StatusCode::NOT_FOUND,
      StatusCode::GONE,
      StatusCode::SERVICE_UNAVAILABLE,
    ] {
      let response = tauri::http::Response::builder()
        .status(status)
        .body("")
        .unwrap();
      assert_eq!(
        decode_remote(response.into()).await.err().as_deref(),
        Some("Device verification is temporarily unavailable. Try again later.")
      );
    }
  }

  #[test]
  fn managed_pairing_matches_relay_proof_vectors() {
    let vault_id = "00112233-4455-4677-8899-aabbccddeeff".parse().unwrap();
    let root = Zeroizing::new(std::array::from_fn(|index| index as u8));
    let vault = LocalVault::new(vault_id, &root);

    assert_eq!(
      encode_proof(&vault, None).unwrap().as_str(),
      "lwFvvozDsCpFztU4CVt25Ti5AoU67HP07aio1Mqkx7k"
    );
    let approval = ApproveInput {
      pair_id: "a3068f26-2a3a-41dd-8c8a-6f7af0b8912c".parse().unwrap(),
      approval_nonce: URL_SAFE_NO_PAD.encode([0; 16]),
    };
    assert_eq!(
      encode_proof(&vault, Some(&approval)).unwrap().as_str(),
      "GqsUeUMDshZ7kH_rIE5f0gFqCWyPTXiSmhOIuLa2PtQ"
    );
  }

  #[test]
  fn vault_upgrades_preserve_the_existing_key_and_require_verification() {
    let vault = LocalVault::new(Uuid::new_v4(), &crypto::random().unwrap());
    let remote = RemoteVault {
      vault_id: Some(vault.vault_id),
      managed: false,
    };
    assert_eq!(
      unverified_status(Some(&vault), Ok(remote)),
      json!({ "state": "unverified", "action": "upgrade" })
    );
    assert_eq!(unverified_status(None, Ok(remote))["action"], "legacy");
    assert!(!is_verified(&vault));
  }

  #[test]
  fn unavailable_verification_discovery_never_guesses_a_setup_action() {
    let vault = LocalVault::new(Uuid::new_v4(), &crypto::random().unwrap());
    for local in [None, Some(&vault)] {
      assert_eq!(
        unverified_status(local, Err("Could not reach the relay.".into())),
        json!({"state":"unverified", "action":"check", "error":"Could not reach the relay."}),
      );
    }
  }

  #[test]
  fn verification_opens_only_the_account_authorization_endpoint() {
    let account = Url::parse("https://account.example.invalid").unwrap();
    let allowed = account.join("/oidc/auth?client_id=fixture").unwrap();
    assert!(valid_authorization_url(&allowed, &account));
    assert!(!valid_authorization_url(
      &account.join("/api/v1/key-vault").unwrap(),
      &account
    ));
    let other = Url::parse("https://other.example.invalid/oidc/auth").unwrap();
    assert!(!valid_authorization_url(&other, &account));
  }

  #[test]
  fn pairing_accepts_six_digits_and_a_canonical_nonce() {
    let mut pair = Pair {
      pair_id: Uuid::new_v4(),
      code: "001234".into(),
      expires_at: 10,
      approved: false,
      approval_nonce: URL_SAFE_NO_PAD.encode([0; 16]),
    };
    assert!(pair.valid(9));
    assert!(!pair.valid(10));
    pair.code = "１２３４５６".into();
    assert!(!pair.valid(9));
  }
}

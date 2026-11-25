use super::{
  acknowledged, automatic, crypto, decode, kick, local_vault, save_vault, unlocked, verify_account,
  Access, AppHandle, Host, LocalVault, Method, Secret, LOCKED,
};
use apell_vault_keys::derive_pairing_proof_key;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::hmac;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::Url;
use tauri_plugin_opener::OpenerExt;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use uuid::Uuid;
use zeroize::Zeroizing;

const MANAGED: &str = "/api/v1/key-vault/managed";
const EXPIRED: &str = "This request expired. Start again.";

#[derive(Default)]
pub(super) struct Managed {
  joining: Mutex<Option<Joining>>,
  recovery: Mutex<Option<Recovery>>,
}

impl Managed {
  pub async fn clear(&self) {
    *self.joining.lock().await = None;
    *self.recovery.lock().await = None;
  }
}

struct Joining {
  account: String,
  vault_id: Uuid,
  pair: Pair,
  secret: Secret,
}

struct Recovery {
  account: String,
  vault_id: Uuid,
  challenge: Challenge,
  secret: Secret,
  enrolling: bool,
}

#[derive(Deserialize)]
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
  Complete { vault_key: Option<String> },
}

pub(super) async fn remote(host: &Host, access: &Access) -> Result<RemoteVault, String> {
  let response = host
    .key_sync
    .request(access, Method::GET, MANAGED, None)
    .await?;
  decode(response).await
}

fn is_verified(local: &LocalVault) -> bool {
  local.managed && !local.pending_managed && local.setup.is_none()
}

pub(super) async fn status(app: &AppHandle, host: &Host) -> Result<Value, String> {
  if !host.account.key_exchange_available() {
    return Ok(json!({ "state": "unavailable" }));
  }
  let Some(account) = host.account.account_key(app).await? else {
    return Ok(json!({ "state": "signed_out" }));
  };
  let local = local_vault(app, &account).await?;
  if local.as_ref().is_some_and(is_verified) {
    return Ok(json!({
      "state": "verified",
      "sync_error": host.key_sync.automatic.error(&account).await,
    }));
  }
  let access = host.account.key_exchange_access(app).await?;
  if access.account_key != account {
    return Err("The signed-in Account changed. Try again.".into());
  }
  let remote = remote(host, &access).await?;
  Ok(unverified_status(local.as_ref(), &remote))
}

fn unverified_status(local: Option<&LocalVault>, remote: &RemoteVault) -> Value {
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

async fn activate(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  mut vault: LocalVault,
) -> Result<(), String> {
  verify_account(app, host, &access.account_key).await?;
  vault.managed = true;
  vault.pending_managed = true;
  vault.setup = None;
  save_vault(app, &access.account_key, &vault).await?;
  automatic::activate(app, host, &access.account_key, vault.vault_id).await?;
  vault.pending_managed = false;
  save_vault(app, &access.account_key, &vault).await?;
  kick(app);
  Ok(())
}

pub(super) async fn create(app: &AppHandle, host: &Host, access: &Access) -> Result<Value, String> {
  let remote = remote(host, access).await?;
  let saved = local_vault(app, &access.account_key).await?;
  let mut vault = match (remote.vault_id, saved) {
    (Some(id), Some(saved)) if remote.managed && saved.vault_id == id => saved,
    (Some(_), _) => return Err("Pair this device with the existing encryption vault.".into()),
    (None, Some(saved)) if saved.pending_managed || saved.setup.is_some() => saved,
    (None, Some(_)) => return Err("The saved vault is missing from Account.".into()),
    (None, None) => LocalVault::new(Uuid::new_v4(), &crypto::random()?),
  };
  vault.pending_managed = true;
  verify_account(app, host, &access.account_key).await?;
  save_vault(app, &access.account_key, &vault).await?;
  let response = host
    .key_sync
    .request(
      access,
      Method::PUT,
      MANAGED,
      Some(json!({
        "vault_id": vault.vault_id, "vault_key": encode(&vault.key()?).as_str(),
      })),
    )
    .await?;
  acknowledged(response)?;
  activate(app, host, access, vault).await?;
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

pub(super) async fn start_pair(host: &Host, access: &Access) -> Result<Value, String> {
  let vault = remote(host, access).await?;
  let vault_id = vault.vault_id.filter(|_| vault.managed).ok_or(LOCKED)?;
  let pair_id = Uuid::new_v4();
  let secret = crypto::random()?;
  let response = host
    .key_sync
    .request(
      access,
      Method::PUT,
      &format!("{MANAGED}/pairs/{pair_id}"),
      Some(json!({
        "vault_id": vault_id, "request_secret": encode(&secret).as_str(),
      })),
    )
    .await?;
  let pair: Pair = decode(response).await?;
  if pair.pair_id != pair_id || pair.approved || !pair.valid(now()) {
    return Err("Account returned an invalid pairing request.".into());
  }
  let display = serde_json::to_value(&pair).map_err(|_| "Could not show the pairing code.")?;
  *host.key_sync.managed.joining.lock().await = Some(Joining {
    account: access.account_key.clone(),
    vault_id,
    pair,
    secret,
  });
  Ok(display)
}

pub(super) async fn list_pairs(
  app: &AppHandle,
  host: &Host,
  access: &Access,
) -> Result<Value, String> {
  let vault = unlocked(app, host, access).await?;
  let proof = encode_proof(&vault, None)?;
  let response = host
    .key_sync
    .request(
      access,
      Method::POST,
      &format!("{MANAGED}/pairs/list"),
      Some(json!({ "vault_id": vault.vault_id, "proof": proof.as_str() })),
    )
    .await?;
  let pairs: PairList = decode(response).await?;
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
) -> Result<Value, String> {
  let vault = unlocked(app, host, access).await?;
  let proof = encode_proof(&vault, Some(&input))?;
  let response = host
    .key_sync
    .request(
      access,
      Method::POST,
      &format!("{MANAGED}/pairs/{}/approve", input.pair_id),
      Some(json!({ "vault_id": vault.vault_id, "proof": proof.as_str() })),
    )
    .await?;
  acknowledged(response)?;
  Ok(Value::Null)
}

fn joining<'a>(
  stored: &'a Option<Joining>,
  access: &Access,
  pair_id: Uuid,
) -> Result<&'a Joining, String> {
  stored
    .as_ref()
    .filter(|pending| {
      pending.account == access.account_key
        && pending.pair.pair_id == pair_id
        && pending.pair.valid(now())
    })
    .ok_or_else(|| EXPIRED.into())
}

async fn consume_pair(host: &Host, access: &Access, pending: &Joining) {
  let _ = host
    .key_sync
    .request(
      access,
      Method::POST,
      &format!("{MANAGED}/pairs/{}/consume", pending.pair.pair_id),
      Some(json!({ "request_secret": encode(&pending.secret).as_str() })),
    )
    .await;
}

pub(super) async fn finish_pair(
  app: &AppHandle,
  host: &Host,
  access: &Access,
  input: PairInput,
) -> Result<Value, String> {
  let mut pending = host.key_sync.managed.joining.lock().await;
  let request = joining(&pending, access, input.pair_id)?;
  let response = host
    .key_sync
    .request(
      access,
      Method::POST,
      &format!("{MANAGED}/pairs/{}/finish", input.pair_id),
      Some(json!({ "request_secret": encode(&request.secret).as_str() })),
    )
    .await?;
  let result: PairResult = decode(response).await?;
  let Some(key) = result.vault_key.map(Zeroizing::new) else {
    return Ok(json!(false));
  };
  let key = crypto::decode_key(&key).ok_or("Account returned an invalid vault key.")?;
  let vault = LocalVault::new(request.vault_id, &key);
  activate(app, host, access, vault).await?;
  consume_pair(host, access, request).await;
  *pending = None;
  Ok(json!(true))
}

pub(super) async fn cancel_pair(
  host: &Host,
  access: &Access,
  input: PairInput,
) -> Result<Value, String> {
  let mut pending = host.key_sync.managed.joining.lock().await;
  if let Ok(request) = joining(&pending, access, input.pair_id) {
    consume_pair(host, access, request).await;
    *pending = None;
  }
  Ok(Value::Null)
}

pub(super) async fn start_recovery(
  app: &AppHandle,
  host: &Host,
  access: &Access,
) -> Result<Value, String> {
  let remote = remote(host, access).await?;
  let vault_id = remote.vault_id.ok_or(LOCKED)?;
  let enrolling = !remote.managed;
  if enrolling
    && !local_vault(app, &access.account_key)
      .await?
      .is_some_and(|vault| vault.vault_id == vault_id)
  {
    return Err("Open Apell on a device with this vault to finish its recovery setup.".into());
  }
  let secret = crypto::random()?;
  let request_secret_hash = URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_ref()));
  let response = host.key_sync.request(access, Method::POST, &format!("{MANAGED}/recovery/start"), Some(json!({
    "vault_id": vault_id, "purpose": if enrolling { "enroll" } else { "recover" }, "request_secret_hash": request_secret_hash,
  }))).await?;
  let challenge: Challenge = decode(response).await?;
  if crypto::decode_key(&challenge.recovery_id).is_none()
    || challenge.expires_at <= now()
    || !valid_authorization_url(&challenge.authorization_url, &access.sync_url)
  {
    return Err("Account returned an invalid verification request.".into());
  }
  let expires_at = challenge.expires_at;
  *host.key_sync.managed.recovery.lock().await = Some(Recovery {
    account: access.account_key.clone(),
    vault_id,
    challenge,
    secret,
    enrolling,
  });
  Ok(json!({ "expires_at": expires_at }))
}

fn valid_authorization_url(url: &Url, account: &Url) -> bool {
  url.scheme() == "https"
    && url.origin() == account.origin()
    && url.path().ends_with("/oidc/auth")
    && url.username().is_empty()
    && url.password().is_none()
    && url.fragment().is_none()
}

fn recovery<'a>(stored: &'a Option<Recovery>, access: &Access) -> Result<&'a Recovery, String> {
  stored
    .as_ref()
    .filter(|pending| pending.account == access.account_key && pending.challenge.expires_at > now())
    .ok_or_else(|| EXPIRED.into())
}

pub(super) async fn open_recovery(
  app: &AppHandle,
  host: &Host,
  access: &Access,
) -> Result<Value, String> {
  let pending = host.key_sync.managed.recovery.lock().await;
  let request = recovery(&pending, access)?;
  app
    .opener()
    .open_url(request.challenge.authorization_url.as_str(), None::<&str>)
    .map_err(|_| "Could not open Account verification.")?;
  Ok(Value::Null)
}

pub(super) async fn finish_recovery(
  app: &AppHandle,
  host: &Host,
  access: &Access,
) -> Result<Value, String> {
  let mut pending = host.key_sync.managed.recovery.lock().await;
  let request = recovery(&pending, access)?;
  let path = format!(
    "{MANAGED}/recovery/{}/finish",
    request.challenge.recovery_id
  );
  let proof = encode(&request.secret);
  let response = host
    .key_sync
    .request(
      access,
      Method::POST,
      &path,
      Some(json!({ "request_secret": proof.as_str() })),
    )
    .await?;
  let vault = match decode::<RecoveryResult>(response).await? {
    RecoveryResult::Pending => return Ok(json!(false)),
    RecoveryResult::Verified if request.enrolling => {
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
      vault
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
  activate(app, host, access, vault).await?;
  *pending = None;
  Ok(json!(true))
}

pub(super) async fn cancel_recovery(host: &Host) -> Result<Value, String> {
  *host.key_sync.managed.recovery.lock().await = None;
  Ok(Value::Null)
}

fn now() -> i64 {
  OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
  use super::*;

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
      unverified_status(Some(&vault), &remote),
      json!({ "state": "unverified", "action": "upgrade" })
    );
    assert_eq!(unverified_status(None, &remote)["action"], "legacy");
    assert!(!is_verified(&vault));
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

mod database;
#[cfg(test)]
mod tests;

use super::{master_key, Key};
use crate::{
  auth::{account::Account, ConfigSyncAuth},
  key_relay::{now, payload, storage_error, validate_id, Relay, WRITE},
  Result, StoreError,
};
use axum::{
  extract::{rejection::QueryRejection, Path, Query, Request, State},
  http::{header, HeaderValue, StatusCode},
  response::{Html, IntoResponse, Response},
  routing::{get, post},
  Json, Router,
};
use base64::{
  engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
  Engine,
};
use ring::hmac;
use rusqlite::Connection;
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, sync::LazyLock};
use zeroize::{Zeroize, Zeroizing};

const CALLBACK: &str = "/api/v1/key-vault/managed/recovery/callback";
const LIFETIME: i64 = 300;
const INVALID_CONFIG: &str = "Invalid Account recovery OIDC settings file.";
const EXPIRED_CODE: &str = "key_recovery_expired";
const RESULT_STYLES: &str = include_str!("recovery/result.css");
static RESULT_CSP: LazyLock<HeaderValue> = LazyLock::new(|| {
  let hash = STANDARD.encode(Sha256::digest(RESULT_STYLES));
  HeaderValue::from_str(&format!(
    "default-src 'none'; style-src 'sha256-{hash}'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"
  ))
  .expect("a base64 digest is valid in a CSP header")
});

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::key_relay) struct Config {
  client_id: String,
  client_secret: String,
}

impl Config {
  pub(in crate::key_relay) fn from_env() -> std::result::Result<Option<Self>, String> {
    let Some(path) = std::env::var_os("APELL_ACCOUNT_KEY_EXCHANGE_RECOVERY_OIDC_FILE") else {
      return Ok(None);
    };
    let invalid = |_| INVALID_CONFIG.to_owned();
    let file = File::open(path).map_err(invalid)?;
    if !file.metadata().map_err(invalid)?.is_file() {
      return Err(INVALID_CONFIG.to_owned());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(4097).read_to_end(&mut bytes).map_err(invalid)?;
    if bytes.len() > 4096 {
      return Err(INVALID_CONFIG.to_owned());
    }
    serde_json::from_slice(&bytes)
      .map(Some)
      .map_err(|_| INVALID_CONFIG.to_owned())
  }
}

impl Drop for Config {
  fn drop(&mut self) {
    self.client_secret.zeroize();
  }
}

pub(in crate::key_relay) struct Recovery {
  oidc: Account,
}

impl Recovery {
  pub(in crate::key_relay) fn new(mut config: Config, auth: &ConfigSyncAuth) -> Result<Self> {
    if config.client_id == auth.client_id() {
      return Err(StoreError::invalid(
        "Account recovery requires a separate OIDC web client.",
      ));
    }
    Ok(Self {
      oidc: Account::new_recovery(
        auth.issuer(),
        std::mem::take(&mut config.client_id),
        std::mem::take(&mut config.client_secret),
        CALLBACK,
      )?,
    })
  }
}

pub(super) fn routes() -> Router<Relay> {
  Router::new()
    .route("/api/v1/key-vault/managed/recovery/start", post(start))
    .route(
      "/api/v1/key-vault/managed/recovery/{recovery_id}/finish",
      post(finish),
    )
    .route(CALLBACK, get(callback))
}

pub(in crate::key_relay) fn migrate(db: &Connection) -> Result<()> {
  database::migrate(db)
}

pub(in crate::key_relay) fn prune(db: &Connection, time: i64) -> Result<()> {
  database::prune(db, time)
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Purpose {
  Recover,
  Enroll,
}

impl Purpose {
  fn as_str(self) -> &'static str {
    match self {
      Self::Recover => "recover",
      Self::Enroll => "enroll",
    }
  }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartInput {
  vault_id: String,
  purpose: Purpose,
  request_secret_hash: String,
}

#[derive(Serialize)]
struct StartReply {
  recovery_id: String,
  authorization_url: String,
  expires_at: i64,
}

async fn start(State(relay): State<Relay>, request: Request) -> Result<Json<StartReply>> {
  let owner = relay.owner(request.headers(), WRITE).await?;
  let recovery = relay.recovery.as_ref().ok_or_else(unavailable)?;
  let master = master_key(&relay)?;
  let input: StartInput = payload(request).await?;
  validate_id(&input.vault_id)?;
  let request_hash = decode_secret(&input.request_secret_hash)?;
  let recovery_id = URL_SAFE_NO_PAD.encode(random_secret()?.as_ref());
  let raw_state = random_secret()?;
  let state = Zeroizing::new(URL_SAFE_NO_PAD.encode(raw_state.as_ref()));
  let state_hash: [u8; 32] = Sha256::digest(raw_state.as_ref()).into();
  let proof = BrowserProof::new(&master, &raw_state);
  let authorization_url = recovery
    .oidc
    .recovery_url(&state, &proof.verifier, &proof.nonce)
    .await?;
  let started_at = now();
  let expires_at = started_at + LIFETIME;
  let id = recovery_id.clone();
  relay
    .query(move |db| {
      database::start(
        db,
        &owner,
        database::NewRequest {
          id: &id,
          vault_id: &input.vault_id,
          purpose: input.purpose,
          request_hash: request_hash.as_ref(),
          state_hash: &state_hash,
          started_at,
          expires_at,
        },
      )
    })
    .await?;
  Ok(Json(StartReply {
    recovery_id,
    authorization_url: authorization_url.into(),
    expires_at,
  }))
}

#[derive(Deserialize)]
struct CallbackInput {
  state: Option<String>,
  code: Option<String>,
  error: Option<String>,
  iss: Option<String>,
}

async fn callback(
  State(relay): State<Relay>,
  input: std::result::Result<Query<CallbackInput>, QueryRejection>,
) -> Response {
  let result = match input {
    Ok(Query(input)) => verify_callback(&relay, input).await,
    Err(_) => Err(expired()),
  };
  let (status, title, message) = match result {
    Ok(()) => (
      StatusCode::OK,
      "Account verified",
      "Return to Apell to finish verifying this device. You can close this browser tab.",
    ),
    Err(error) if error.code == EXPIRED_CODE => (
      StatusCode::BAD_REQUEST,
      "Verification request unavailable",
      "This request may have expired or already been used. Return to Apell to start a new one.",
    ),
    Err(_) => (
      StatusCode::BAD_REQUEST,
      "Account verification incomplete",
      "Return to Apell and try again. When prompted, sign in with the same Apell Account.",
    ),
  };
  let body = format!(
    include_str!("recovery/result.html"),
    styles = RESULT_STYLES,
    title = title,
    message = message,
  );
  let mut response = (status, Html(body)).into_response();
  response
    .headers_mut()
    .insert(header::CONTENT_SECURITY_POLICY, RESULT_CSP.clone());
  response
}

async fn verify_callback(relay: &Relay, input: CallbackInput) -> Result<()> {
  let recovery = relay.recovery.as_ref().ok_or_else(unavailable)?;
  let master = master_key(relay)?;
  let raw_state = input
    .state
    .as_deref()
    .and_then(|state| decode_secret(state).ok())
    .ok_or_else(expired)?;
  if input
    .iss
    .as_deref()
    .is_some_and(|issuer| issuer != relay.auth.issuer())
  {
    return Err(expired());
  }
  let state_hash: [u8; 32] = Sha256::digest(raw_state.as_ref()).into();
  let issuer = relay.auth.issuer().to_owned();
  let challenge = relay
    .query(move |db| database::take_browser_state(db, &issuer, &state_hash, now()))
    .await?;
  let proof = BrowserProof::new(&master, &raw_state);
  let verification = if let Some(code) = input.code.filter(|_| input.error.is_none()) {
    recovery
      .oidc
      .verify_recovery(
        &code,
        &proof.verifier,
        &proof.nonce,
        &challenge.subject,
        challenge.started_at,
      )
      .await
  } else {
    Err(StoreError::invalid(
      "Account verification did not complete.",
    ))
  };
  let verified = verification.is_ok();
  relay
    .query(move |db| database::mark_verified(db, &challenge.id, verified, now()))
    .await?;
  verification
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FinishInput {
  request_secret: String,
  vault_key: Option<String>,
}

impl Drop for FinishInput {
  fn drop(&mut self) {
    self.request_secret.zeroize();
    self.vault_key.zeroize();
  }
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum FinishReply {
  Pending,
  Verified,
  Complete {
    #[serde(skip_serializing_if = "Option::is_none")]
    vault_key: Option<VaultKey>,
  },
}

struct VaultKey(Key);

impl Serialize for VaultKey {
  fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
    let encoded = Zeroizing::new(URL_SAFE_NO_PAD.encode(self.0.as_ref()));
    serializer.serialize_str(&encoded)
  }
}

async fn finish(
  State(relay): State<Relay>,
  Path(id): Path<String>,
  request: Request,
) -> Result<Json<FinishReply>> {
  let owner = relay.owner(request.headers(), WRITE).await?;
  if relay.recovery.is_none() {
    return Err(unavailable());
  }
  let master = master_key(&relay)?;
  decode_secret(&id).map_err(|_| expired())?;
  let input: FinishInput = payload(request).await?;
  let request_hash: [u8; 32] =
    Sha256::digest(decode_secret(&input.request_secret)?.as_ref()).into();
  let vault_key = input.vault_key.as_deref().map(decode_secret).transpose()?;
  let stage = relay
    .query(move |db| {
      database::finish(
        db,
        &master,
        &owner,
        &id,
        &request_hash,
        vault_key.as_ref(),
        now(),
      )
    })
    .await?;
  let reply = match stage {
    database::Stage::Pending => FinishReply::Pending,
    database::Stage::Verified => FinishReply::Verified,
    database::Stage::Complete(vault_key) => FinishReply::Complete {
      vault_key: vault_key.map(VaultKey),
    },
  };
  Ok(Json(reply))
}

struct BrowserProof {
  verifier: Zeroizing<String>,
  nonce: Zeroizing<String>,
}

impl BrowserProof {
  fn new(master: &Key, state: &Key) -> Self {
    // The DB stores a hash of state. The relay rebuilds each value from
    // the browser's high-entropy state using a distinct HMAC domain.
    let secret = hmac::Key::new(hmac::HMAC_SHA256, master.as_ref());
    let derive = |label: &[u8]| {
      let mut context = hmac::Context::with_key(&secret);
      context.update(label);
      context.update(state.as_ref());
      Zeroizing::new(URL_SAFE_NO_PAD.encode(context.sign().as_ref()))
    };
    Self {
      verifier: derive(b"apell-key-recovery-pkce-v1\0"),
      nonce: derive(b"apell-key-recovery-nonce-v1\0"),
    }
  }
}

fn random_secret() -> Result<Key> {
  let mut bytes = Zeroizing::new([0; 32]);
  getrandom::fill(bytes.as_mut()).map_err(|_| storage_error())?;
  Ok(bytes)
}

fn decode_secret(value: &str) -> Result<Key> {
  let mut decoded = Zeroizing::new([0; 32]);
  if value.len() != 43 || URL_SAFE_NO_PAD.decode_slice(value, decoded.as_mut()).ok() != Some(32) {
    return Err(StoreError::invalid("Invalid Account recovery request."));
  }
  Ok(decoded)
}

fn unavailable() -> StoreError {
  StoreError::new(
    StatusCode::SERVICE_UNAVAILABLE,
    "key_recovery_unavailable",
    "Account key recovery is unavailable.",
  )
}

fn expired() -> StoreError {
  StoreError::new(
    StatusCode::NOT_FOUND,
    EXPIRED_CODE,
    "The Account verification request is unavailable or has expired.",
  )
}

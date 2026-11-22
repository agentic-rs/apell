//! Account-side relay for encrypted vault keys. It runs independently of the Store.

mod database;
#[cfg(test)]
mod tests;

use crate::{auth::ConfigSyncAuth, ConfigSyncConfig, Result, StoreError};
use axum::{
  body::to_bytes,
  extract::{Path, Request, State},
  http::{header, HeaderMap, HeaderValue, StatusCode},
  middleware::{self, Next},
  response::Response,
  routing::get,
  Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::Connection;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{
  net::SocketAddr,
  path::PathBuf,
  sync::{Arc, Mutex},
  time::Duration,
};
use tokio::sync::Semaphore;

const READ: &str = "key_exchange:read";
const WRITE: &str = "key_exchange:write";
const MAX_BODY_BYTES: usize = 1024;

pub struct Config {
  pub listen: SocketAddr,
  data_dir: PathBuf,
  authentication: ConfigSyncConfig,
}

impl Config {
  pub fn from_env() -> std::result::Result<Self, String> {
    let listen = std::env::var("APELL_ACCOUNT_KEY_EXCHANGE_LISTEN")
      .unwrap_or_else(|_| "127.0.0.1:5657".into())
      .parse()
      .map_err(|_| "APELL_ACCOUNT_KEY_EXCHANGE_LISTEN must be an IP address and port.")?;
    let data_dir = std::env::var_os("APELL_ACCOUNT_KEY_EXCHANGE_DATA_DIR")
      .filter(|value| !value.is_empty())
      .map(PathBuf::from)
      .ok_or("Set APELL_ACCOUNT_KEY_EXCHANGE_DATA_DIR.")?;
    let file = std::env::var_os("APELL_ACCOUNT_KEY_EXCHANGE_FILE")
      .filter(|value| !value.is_empty())
      .map(PathBuf::from)
      .ok_or("Set APELL_ACCOUNT_KEY_EXCHANGE_FILE.")?;
    let authentication = ConfigSyncConfig::from_file(&file)
      .map_err(|_| "Invalid Account key exchange OIDC settings file.")?;
    Ok(Self {
      listen,
      data_dir,
      authentication,
    })
  }
}

#[derive(Clone)]
pub struct Relay {
  auth: Arc<ConfigSyncAuth>,
  database: Arc<Mutex<Connection>>,
  queries: Arc<Semaphore>,
}

impl Relay {
  pub fn open(config: Config) -> Result<Self> {
    Ok(Self {
      auth: Arc::new(ConfigSyncAuth::new(config.authentication)?),
      database: Arc::new(Mutex::new(database::open(&config.data_dir)?)),
      queries: Arc::new(Semaphore::new(4)),
    })
  }

  pub fn router(&self) -> Router {
    Router::new()
      .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
      .route("/api/v1/key-vault", get(read_vault).put(create_vault))
      .route(
        "/api/v1/key-vault/pairs/{pair_id}",
        get(read_pair).put(create_pair).delete(consume_pair),
      )
      .route(
        "/api/v1/key-vault/pairs/{pair_id}/envelope",
        axum::routing::put(publish_pair),
      )
      .layer(middleware::from_fn(response_headers))
      .with_state(self.clone())
  }

  pub async fn prune_expired(&self) -> Result<()> {
    self.query(|db| database::prune_expired(db, now())).await
  }

  async fn owner(&self, headers: &HeaderMap, scope: &str) -> Result<Owner> {
    let subject =
      self
        .auth
        .authorize(headers, scope)
        .await
        .map_err(|error| match error.status {
          StatusCode::UNAUTHORIZED => StoreError::new(
            StatusCode::UNAUTHORIZED,
            "key_exchange_sign_in_required",
            "Sign in to Apell Account to manage encrypted keys.",
          ),
          StatusCode::FORBIDDEN => StoreError::new(
            StatusCode::FORBIDDEN,
            "key_exchange_scope_required",
            "This Account cannot perform this key exchange operation.",
          ),
          StatusCode::TOO_MANY_REQUESTS => busy(),
          _ => error,
        })?;
    Ok(Owner {
      issuer: self.auth.issuer().into(),
      subject,
    })
  }

  async fn query<T: Send + 'static>(
    &self,
    operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
  ) -> Result<T> {
    let permit = self
      .queries
      .clone()
      .try_acquire_owned()
      .map_err(|_| busy())?;
    let database = self.database.clone();
    tokio::task::spawn_blocking(move || {
      let _permit = permit;
      let mut connection = database.lock().map_err(|_| storage_error())?;
      operation(&mut connection)
    })
    .await
    .map_err(|_| storage_error())?
  }
}

struct Owner {
  issuer: String,
  subject: String,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Envelope {
  nonce: String,
  ciphertext: String,
}

impl Envelope {
  fn validate(&self) -> Result<()> {
    // ChaCha20-Poly1305 seals a 32-byte vault key with a 16-byte tag.
    if !valid_encoded::<12>(&self.nonce) || !valid_encoded::<48>(&self.ciphertext) {
      return Err(StoreError::invalid("Invalid encrypted vault key envelope."));
    }
    Ok(())
  }
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Vault {
  vault_id: String,
  recovery: Envelope,
}

#[derive(Serialize)]
struct VaultStatus {
  vault: Option<Vault>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairRequest {
  vault_id: String,
}

#[derive(Serialize)]
struct PairStatus {
  vault_id: String,
  envelope: Option<Envelope>,
}

async fn response_headers(request: Request, next: Next) -> Response {
  let mut response = next.run(request).await;
  response
    .headers_mut()
    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
  response.headers_mut().insert(
    header::REFERRER_POLICY,
    HeaderValue::from_static("no-referrer"),
  );
  response
}

async fn read_vault(State(relay): State<Relay>, headers: HeaderMap) -> Result<Json<VaultStatus>> {
  let owner = relay.owner(&headers, READ).await?;
  relay
    .query(move |db| {
      Ok(Json(VaultStatus {
        vault: database::vault(db, &owner)?,
      }))
    })
    .await
}

async fn create_vault(State(relay): State<Relay>, request: Request) -> Result<StatusCode> {
  let owner = relay.owner(request.headers(), WRITE).await?;
  let vault: Vault = payload(request).await?;
  validate_id(&vault.vault_id)?;
  vault.recovery.validate()?;
  relay
    .query(move |db| database::create_vault(db, &owner, &vault))
    .await?;
  Ok(StatusCode::NO_CONTENT)
}

async fn create_pair(
  State(relay): State<Relay>,
  Path(pair_id): Path<String>,
  request: Request,
) -> Result<StatusCode> {
  let owner = relay.owner(request.headers(), WRITE).await?;
  validate_id(&pair_id)?;
  let pair: PairRequest = payload(request).await?;
  validate_id(&pair.vault_id)?;
  relay
    .query(move |db| database::create_pair(db, &owner, &pair_id, &pair.vault_id, now()))
    .await?;
  Ok(StatusCode::NO_CONTENT)
}

async fn read_pair(
  State(relay): State<Relay>,
  Path(pair_id): Path<String>,
  headers: HeaderMap,
) -> Result<Json<PairStatus>> {
  let owner = relay.owner(&headers, READ).await?;
  validate_id(&pair_id)?;
  relay
    .query(move |db| database::pair(db, &owner, &pair_id, now()).map(Json))
    .await
}

async fn publish_pair(
  State(relay): State<Relay>,
  Path(pair_id): Path<String>,
  request: Request,
) -> Result<StatusCode> {
  let owner = relay.owner(request.headers(), WRITE).await?;
  validate_id(&pair_id)?;
  let envelope: Envelope = payload(request).await?;
  envelope.validate()?;
  relay
    .query(move |db| database::publish_pair(db, &owner, &pair_id, &envelope, now()))
    .await?;
  Ok(StatusCode::NO_CONTENT)
}

async fn consume_pair(
  State(relay): State<Relay>,
  Path(pair_id): Path<String>,
  headers: HeaderMap,
) -> Result<StatusCode> {
  let owner = relay.owner(&headers, WRITE).await?;
  validate_id(&pair_id)?;
  relay
    .query(move |db| database::consume_pair(db, &owner, &pair_id, now()))
    .await?;
  Ok(StatusCode::NO_CONTENT)
}

async fn payload<T: DeserializeOwned>(request: Request) -> Result<T> {
  let content_type = request
    .headers()
    .get(header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok());
  if !content_type
    .and_then(|value| value.split(';').next())
    .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
  {
    return Err(StoreError::invalid(
      "Send the key exchange request as application/json.",
    ));
  }
  let body = tokio::time::timeout(
    Duration::from_secs(10),
    to_bytes(request.into_body(), MAX_BODY_BYTES),
  )
  .await
  .map_err(|_| StoreError::invalid("The key exchange request timed out."))?
  .map_err(|_| {
    StoreError::invalid("Could not read the key exchange request within its size limit.")
  })?;
  serde_json::from_slice(&body).map_err(|_| StoreError::invalid("Invalid key exchange request."))
}

fn validate_id(value: &str) -> Result<()> {
  if value.len() != 36
    || !value.bytes().enumerate().all(|(index, byte)| match index {
      8 | 13 | 18 | 23 => byte == b'-',
      14 => byte == b'4',
      19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
      _ => byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'),
    })
  {
    return Err(StoreError::invalid("Invalid vault or pairing identifier."));
  }
  Ok(())
}

fn valid_encoded<const BYTES: usize>(value: &str) -> bool {
  value.len() == (BYTES * 4).div_ceil(3)
    && URL_SAFE_NO_PAD
      .decode_slice(value, &mut [0u8; BYTES])
      .is_ok_and(|decoded| decoded == BYTES)
}

fn now() -> i64 {
  chrono::Utc::now().timestamp()
}

fn busy() -> StoreError {
  StoreError::new(
    StatusCode::TOO_MANY_REQUESTS,
    "key_exchange_busy",
    "Key exchange is busy. Try again shortly.",
  )
}

fn storage_error() -> StoreError {
  StoreError::new(
    StatusCode::INTERNAL_SERVER_ERROR,
    "key_exchange_storage_error",
    "Could not complete the key exchange operation.",
  )
}

fn conflict(message: &'static str) -> StoreError {
  StoreError::new(StatusCode::CONFLICT, "key_exchange_conflict", message)
}

fn missing_pair() -> StoreError {
  StoreError::new(
    StatusCode::NOT_FOUND,
    "pairing_unavailable",
    "The pairing request is unavailable or has expired.",
  )
}

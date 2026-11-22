use super::{
  account::{keys, parse_url, read_json, validate_issuer, visible_ascii, Audience},
  now,
};
use crate::{Result, StoreError};
use axum::http::{header, HeaderMap, StatusCode};
use jsonwebtoken::{Algorithm, Validation};
use reqwest::Client;
use serde::Deserialize;
use std::{
  io::Read,
  time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};
use url::Url;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSyncConfig {
  issuer: String,
  audience: String,
  client_id: String,
}

impl ConfigSyncConfig {
  pub(crate) fn from_env() -> std::result::Result<Option<Self>, String> {
    let Some(path) = std::env::var_os("APELL_STORE_CONFIG_SYNC_FILE") else {
      return Ok(None);
    };
    let invalid = || "Invalid config sync settings file.".to_owned();
    let file = std::fs::File::open(path).map_err(|_| invalid())?;
    let metadata = file.metadata().map_err(|_| invalid())?;
    if !metadata.is_file() || metadata.len() > 4096 {
      return Err(invalid());
    }
    let mut bytes = Vec::new();
    file
      .take(4097)
      .read_to_end(&mut bytes)
      .map_err(|_| invalid())?;
    if bytes.len() > 4096 {
      return Err(invalid());
    }
    let config: Self = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    config.validate().map_err(|_| invalid())?;
    Ok(Some(config))
  }

  fn validate(&self) -> Result<()> {
    if !validate_issuer(&self.issuer)
      || parse_url(&self.audience).is_none()
      || !visible_ascii(&self.client_id, 128)
      || !self
        .client_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
      return Err(StoreError::invalid(
        "Invalid config sync issuer, audience or client ID.",
      ));
    }
    Ok(())
  }
}

pub(crate) struct ConfigSyncAuth {
  config: ConfigSyncConfig,
  client: Client,
  discovery_url: Url,
  jwks: Mutex<Option<(Url, Instant)>>,
  keys: Mutex<keys::Cache>,
  requests: Semaphore,
}

#[derive(Deserialize)]
struct Discovery {
  issuer: String,
  jwks_uri: String,
}

#[derive(Deserialize)]
struct Claims {
  sub: String,
  aud: Audience,
  exp: i64,
  iat: i64,
  client_id: String,
  scope: String,
}

impl ConfigSyncAuth {
  pub(crate) fn new(config: ConfigSyncConfig) -> Result<Self> {
    config.validate()?;
    let mut discovery_url = parse_url(&config.issuer).ok_or_else(unavailable)?;
    discovery_url
      .path_segments_mut()
      .map_err(|_| unavailable())?
      .pop_if_empty()
      .extend([".well-known", "openid-configuration"]);
    let mut builder = Client::builder()
      .user_agent("Apell-Store")
      .redirect(reqwest::redirect::Policy::none())
      .connect_timeout(Duration::from_secs(5))
      .timeout(Duration::from_secs(10));
    if cfg!(test) && discovery_url.scheme() == "http" {
      builder = builder.no_proxy();
    }
    Ok(Self {
      config,
      client: builder.build().map_err(|_| unavailable())?,
      discovery_url,
      jwks: Mutex::new(None),
      keys: Mutex::new(keys::Cache::default()),
      requests: Semaphore::new(8),
    })
  }

  pub(crate) fn issuer(&self) -> &str {
    &self.config.issuer
  }

  pub(crate) async fn authorize(&self, headers: &HeaderMap, scope: &str) -> Result<String> {
    if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
      return Err(unauthorized());
    }
    let token = headers
      .get(header::AUTHORIZATION)
      .and_then(|value| value.to_str().ok())
      .and_then(|value| value.strip_prefix("Bearer "))
      .filter(|token| visible_ascii(token, 16 * 1024))
      .ok_or_else(unauthorized)?;
    let header = jsonwebtoken::decode_header(token).map_err(|_| unauthorized())?;
    if !matches!(
      header.alg,
      Algorithm::ES256 | Algorithm::ES384 | Algorithm::RS256
    ) || header.jwk.is_some()
      || header.jku.is_some()
      || header.x5u.is_some()
      || header.x5c.is_some()
      || header.crit.is_some()
      || header.enc.is_some()
      || header.zip.is_some()
    {
      return Err(unauthorized());
    }
    let kid = header
      .kid
      .as_deref()
      .filter(|kid| visible_ascii(kid, 128))
      .ok_or_else(unauthorized)?;
    let _permit = self
      .requests
      .try_acquire()
      .map_err(|_| StoreError::busy())?;
    let jwks = self.jwks_uri().await?;
    let key = self
      .keys
      .lock()
      .await
      .get(&self.client, &jwks, kid, header.alg)
      .await
      .map_err(|error| {
        if error.status.is_server_error() {
          unavailable()
        } else {
          unauthorized()
        }
      })?;
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[&self.config.issuer]);
    validation.set_audience(&[&self.config.audience]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation.validate_nbf = true;
    validation.leeway = 0;
    let claims = jsonwebtoken::decode::<Claims>(token, &key, &validation)
      .map_err(|_| unauthorized())?
      .claims;
    let time = now();
    if claims.client_id != self.config.client_id
      || claims.sub == claims.client_id
      || !claims.aud.is_for(&self.config.audience)
      || !visible_ascii(&claims.sub, 255)
      || claims.iat <= 0
      || claims.iat > time.saturating_add(60)
      || claims.iat >= claims.exp
      || claims.exp <= time
    {
      return Err(unauthorized());
    }
    if !claims
      .scope
      .split_ascii_whitespace()
      .any(|granted| granted == scope)
    {
      return Err(StoreError::new(
        StatusCode::FORBIDDEN,
        "sync_scope_required",
        "This Account has no access to this config sync operation.",
      ));
    }
    Ok(claims.sub)
  }

  async fn jwks_uri(&self) -> Result<Url> {
    let mut cache = self.jwks.lock().await;
    if let Some((url, _)) = cache
      .as_ref()
      .filter(|(_, time)| time.elapsed() < Duration::from_secs(300))
    {
      return Ok(url.clone());
    }
    let discovery: Discovery = read_json(self.client.get(self.discovery_url.clone()))
      .await
      .map_err(|_| unavailable())?;
    if discovery.issuer != self.config.issuer {
      return Err(unavailable());
    }
    let issuer = parse_url(&self.config.issuer).ok_or_else(unavailable)?;
    let prefix = format!("{}/", issuer.path().trim_end_matches('/'));
    let jwks = parse_url(&discovery.jwks_uri)
      .filter(|url| url.origin() == issuer.origin() && url.path().starts_with(&prefix))
      .ok_or_else(unavailable)?;
    *cache = Some((jwks.clone(), Instant::now()));
    Ok(jwks)
  }
}

fn unauthorized() -> StoreError {
  StoreError::new(
    StatusCode::UNAUTHORIZED,
    "sync_sign_in_required",
    "Sign in to Apell Account to sync provider settings.",
  )
}

fn unavailable() -> StoreError {
  StoreError::new(
    StatusCode::BAD_GATEWAY,
    "account_unavailable",
    "Account verification is temporarily unavailable.",
  )
}

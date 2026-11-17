mod claims;
mod config;
mod keys;
#[cfg(test)]
mod tests;
mod tokens;

use crate::{Result, StoreError};
use axum::{
  body::to_bytes,
  extract::Request,
  http::{header, HeaderMap, StatusCode},
};
use claims::{Claims, ISSUER};
pub use config::{OidcConfig, TrustedPublisher};
use jsonwebtoken::{decode, decode_header, Algorithm, Validation};
use serde::Deserialize;
use std::{
  sync::{Arc, Mutex},
  time::{SystemTime, UNIX_EPOCH},
};
pub(crate) use tokens::Grant;
use tokens::{TokenResponse, Tokens};
use tokio::sync::Semaphore;

const MAX_JWT_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub(crate) struct Oidc {
  config: OidcConfig,
  keys: Arc<keys::Keys>,
  tokens: Arc<Mutex<Tokens>>,
  exchanges: Arc<Semaphore>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeRequest {
  applet_id: String,
  sha256: String,
}

impl Oidc {
  pub fn new(config: OidcConfig) -> Result<Self> {
    config.validate().map_err(StoreError::invalid)?;
    if jsonwebtoken::crypto::CryptoProvider::try_get_default().is_none() {
      return Err(StoreError::internal());
    }
    Ok(Self {
      config,
      keys: Arc::new(keys::Keys::new()?),
      tokens: Arc::new(Mutex::new(Tokens::default())),
      exchanges: Arc::new(Semaphore::new(2)),
    })
  }

  pub async fn exchange(&self, request: Request) -> Result<TokenResponse> {
    let _permit = self
      .exchanges
      .try_acquire()
      .map_err(|_| StoreError::busy())?;
    let jwt = bearer(request.headers(), MAX_JWT_BYTES)?;
    let header = decode_header(jwt).map_err(|_| unauthorized())?;
    if header.alg != Algorithm::RS256
      || header.typ.as_deref() != Some("JWT")
      || header.jku.is_some()
      || header.jwk.is_some()
      || header.x5u.is_some()
      || header.crit.is_some()
      || header.enc.is_some()
      || header.zip.is_some()
      || !header.extras.inner().is_empty()
    {
      return Err(unauthorized());
    }
    let kid = header
      .kid
      .filter(|value| keys::valid_kid(value))
      .ok_or_else(unauthorized)?;
    let key = self.keys.get(&kid).await?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[&self.config.audience]);
    validation.set_required_spec_claims(&["iss", "aud", "sub", "exp", "nbf"]);
    validation.validate_nbf = true;
    validation.leeway = 30;
    let claims = decode::<Claims>(jwt, &key, &validation)
      .map_err(|_| unauthorized())?
      .claims;
    claims.validate(&self.config.audience, now())?;
    // Verify a trusted identity before polling even the small exchange body.
    if !self
      .config
      .publishers
      .iter()
      .any(|publisher| claims.matches(publisher))
    {
      return Err(unauthorized());
    }
    let content_type = request
      .headers()
      .get(header::CONTENT_TYPE)
      .and_then(|value| value.to_str().ok());
    if !content_type.is_some_and(|value| {
      value
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim() == "application/json")
    }) {
      return Err(StoreError::invalid(
        "Use application/json for trusted publishing.",
      ));
    }
    let bytes = tokio::time::timeout(
      std::time::Duration::from_secs(10),
      to_bytes(request.into_body(), 4096),
    )
    .await
    .map_err(|_| StoreError::invalid("Trusted publishing request timed out."))?
    .map_err(|_| StoreError::invalid("Trusted publishing request exceeds its size limit."))?;
    let input: ExchangeRequest = serde_json::from_slice(&bytes)
      .map_err(|_| StoreError::invalid("Invalid trusted publishing request."))?;
    if !apell_package::valid_name(&input.applet_id)
      || !apell_package::valid_hash(&input.sha256)
      || input.sha256.bytes().any(|byte| byte.is_ascii_uppercase())
    {
      return Err(StoreError::invalid(
        "Invalid applet identity or package checksum.",
      ));
    }
    if !self
      .config
      .publishers
      .iter()
      .any(|publisher| publisher.applet_id == input.applet_id && claims.matches(publisher))
    {
      return Err(unauthorized());
    }
    self
      .tokens
      .lock()
      .map_err(|_| StoreError::internal())?
      .mint(&claims, input.applet_id, input.sha256, now())
  }

  pub fn authenticate(&self, headers: &HeaderMap) -> Result<Grant> {
    let token = bearer(headers, 512)?;
    if !token
      .strip_prefix("apel_ci_")
      .is_some_and(apell_package::valid_hash)
    {
      return Err(unauthorized());
    }
    self
      .tokens
      .lock()
      .map_err(|_| StoreError::internal())?
      .authenticate(token, now())
  }
}

fn bearer(headers: &HeaderMap, maximum: usize) -> Result<&str> {
  if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
    return Err(unauthorized());
  }
  let token = headers
    .get(header::AUTHORIZATION)
    .and_then(|value| value.to_str().ok())
    .and_then(|value| value.strip_prefix("Bearer "))
    .filter(|value| {
      !value.is_empty() && value.len() <= maximum && value.bytes().all(|c| c.is_ascii_graphic())
    })
    .ok_or_else(unauthorized)?;
  Ok(token)
}

pub(crate) fn now() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |duration| duration.as_secs())
}

fn unauthorized() -> StoreError {
  StoreError::new(
    StatusCode::UNAUTHORIZED,
    "unauthorized",
    "Trusted publishing authentication failed.",
  )
}

fn unavailable() -> StoreError {
  StoreError::new(
    StatusCode::SERVICE_UNAVAILABLE,
    "identity_unavailable",
    "GitHub identity verification is temporarily unavailable.",
  )
}

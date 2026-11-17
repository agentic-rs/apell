mod database;
mod routes;
#[cfg(test)]
mod tests;

pub(super) use database::migrate;
pub(super) use routes::routes;

use super::{unauthorized, User};
use crate::{Result, StoreError};
use axum::http::{HeaderMap, StatusCode};
use serde::Serialize;

const FLOW_SECONDS: i64 = 600;
const CREDENTIAL_SECONDS: i64 = 7 * 24 * 60 * 60;
const INTERVAL: i64 = 3;
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Debug, Serialize)]
pub(super) struct Started {
  pub device_code: String,
  pub user_code: String,
  pub expires_at: i64,
  pub interval: i64,
}

#[derive(Debug, Serialize)]
pub(super) struct RequestDetails {
  pub user_code: String,
  pub expires_at: i64,
  pub status: String,
}

#[derive(Debug, Serialize)]
pub(super) struct CredentialDetails {
  pub id: String,
  pub created_at: i64,
  pub expires_at: i64,
}

#[derive(Debug, Serialize)]
pub(super) struct Identity {
  pub user: User,
  pub expires_at: i64,
}

#[derive(Debug, Serialize)]
pub(super) struct Authorized {
  pub token: String,
  pub user: User,
  pub expires_at: i64,
}

#[derive(Debug)]
pub(super) enum Polled {
  Pending,
  Authorized(Authorized),
}

pub(super) fn bearer(headers: &HeaderMap) -> Result<String> {
  if headers.get_all("authorization").iter().count() != 1 {
    return Err(unauthorized());
  }
  let token = headers
    .get("authorization")
    .and_then(|value| value.to_str().ok())
    .and_then(|value| value.strip_prefix("Bearer "))
    .ok_or_else(unauthorized)?;
  let secret = token.strip_prefix("apel_user_").ok_or_else(unauthorized)?;
  if !valid_secret(secret) {
    return Err(unauthorized());
  }
  Ok(token.to_owned())
}

fn valid_secret(value: &str) -> bool {
  value.len() == 64
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn user_code(value: &str) -> Result<String> {
  let normalized = value.trim().to_ascii_uppercase();
  if normalized.len() != 11
    || normalized.as_bytes()[5] != b'-'
    || normalized
      .bytes()
      .enumerate()
      .any(|(i, byte)| i != 5 && !CODE_ALPHABET.contains(&byte))
  {
    return Err(expired());
  }
  Ok(normalized)
}

fn random_code() -> Result<String> {
  let mut bytes = [0; 10];
  getrandom::fill(&mut bytes).map_err(|_| StoreError::internal())?;
  let mut code = String::with_capacity(11);
  for (index, byte) in bytes.iter().enumerate() {
    if index == 5 {
      code.push('-');
    }
    code.push(CODE_ALPHABET[usize::from(*byte) % CODE_ALPHABET.len()] as char);
  }
  Ok(code)
}

fn expired() -> StoreError {
  StoreError::new(
    StatusCode::BAD_REQUEST,
    "authorization_expired",
    "This CLI authorization has expired. Start login again.",
  )
}

fn denied() -> StoreError {
  StoreError::new(
    StatusCode::FORBIDDEN,
    "authorization_denied",
    "CLI access was denied.",
  )
}

fn slow_down() -> StoreError {
  StoreError::new(
    StatusCode::TOO_MANY_REQUESTS,
    "slow_down",
    "Wait before checking this CLI authorization again.",
  )
}

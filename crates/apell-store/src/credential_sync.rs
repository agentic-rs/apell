mod database;

use crate::{query, AppState, Result, StoreError};
use axum::{
  body::to_bytes,
  extract::{Request, State},
  http::{header, HeaderMap, StatusCode},
  routing::get,
  Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, time::Duration};

pub(crate) use database::migrate;

const MAX_BODY_BYTES: usize = 192 * 1024;
const MAX_ENTRIES: usize = 31;
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const MAX_CIPHERTEXT_BYTES: usize = 4096 + TAG_BYTES;

pub(crate) fn routes() -> Router<AppState> {
  Router::new().route("/v1/credential-sync/head", get(head).put(update))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
  expected_head: Option<String>,
  revision_id: String,
  vault_id: String,
  entries: Vec<Entry>,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Entry {
  connection_id: String,
  nonce: String,
  ciphertext: String,
}

#[derive(Serialize)]
struct Head {
  head: Option<Revision>,
}

#[derive(Serialize)]
struct Revision {
  revision_id: String,
  vault_id: String,
  entries: Vec<Entry>,
}

struct Owner {
  issuer: String,
  subject: String,
}

async fn owner(state: &AppState, headers: &HeaderMap, scope: &str) -> Result<Owner> {
  let auth = state.config_sync.as_ref().ok_or_else(|| {
    StoreError::new(
      StatusCode::SERVICE_UNAVAILABLE,
      "credential_sync_unavailable",
      "Credential sync is unavailable on this Store.",
    )
  })?;
  Ok(Owner {
    issuer: auth.issuer().to_owned(),
    subject: auth.authorize(headers, scope).await?,
  })
}

async fn head(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Head>> {
  let owner = owner(&state, &headers, "credentials:read").await?;
  query(&state, move |store| store.credential_sync_head(&owner))
    .await
    .map(Json)
}

async fn update(State(state): State<AppState>, request: Request) -> Result<Json<Head>> {
  let owner = owner(&state, request.headers(), "credentials:write").await?;
  let is_json = request
    .headers()
    .get(header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .and_then(|value| value.split(';').next())
    .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
  if !is_json {
    return Err(StoreError::invalid(
      "Send the encrypted keys as application/json.",
    ));
  }
  let body = tokio::time::timeout(
    Duration::from_secs(15),
    to_bytes(request.into_body(), MAX_BODY_BYTES),
  )
  .await
  .map_err(|_| StoreError::invalid("The credential sync request timed out."))?
  .map_err(|_| StoreError::invalid("Could not read the encrypted keys within their size limit."))?;
  let mut write: Write = serde_json::from_slice(&body)
    .map_err(|_| StoreError::invalid("Invalid credential sync request."))?;
  write.validate()?;
  query(&state, move |store| {
    store.write_credential_sync_head(&owner, write)
  })
  .await
  .map(Json)
}

impl Write {
  fn validate(&mut self) -> Result<()> {
    let mut ids = HashSet::new();
    if !crate::config_sync::revision_id(&self.revision_id)
      || !crate::config_sync::revision_id(&self.vault_id)
      || self
        .expected_head
        .as_ref()
        .is_some_and(|id| !crate::config_sync::revision_id(id) || id == &self.revision_id)
      || self.entries.len() > MAX_ENTRIES
      || self.entries.iter().any(|entry| {
        !crate::config_sync::identifier(&entry.connection_id, 128)
          || !ids.insert(entry.connection_id.as_str())
          || !valid_base64(&entry.nonce, NONCE_BYTES, NONCE_BYTES)
          || !valid_base64(&entry.ciphertext, TAG_BYTES + 1, MAX_CIPHERTEXT_BYTES)
      })
    {
      return Err(StoreError::invalid(
        "Invalid encrypted credential snapshot.",
      ));
    }
    self
      .entries
      .sort_unstable_by(|a, b| a.connection_id.cmp(&b.connection_id));
    Ok(())
  }
}

fn valid_base64(value: &str, min_bytes: usize, max_bytes: usize) -> bool {
  value.len() <= (max_bytes * 4).div_ceil(3)
    && URL_SAFE_NO_PAD
      .decode(value)
      .is_ok_and(|decoded| (min_bytes..=max_bytes).contains(&decoded.len()))
}

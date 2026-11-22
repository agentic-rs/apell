mod database;
#[cfg(test)]
mod tests;

use crate::{query, AppState, Result, StoreError};
use axum::{
  body::to_bytes,
  extract::{Request, State},
  http::{header, HeaderMap, StatusCode},
  routing::get,
  Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, time::Duration};

pub(crate) use database::migrate;

const MAX_BODY_BYTES: usize = 48 * 1024;
const DEMO_ID: &str = "local-demo";

pub(crate) fn routes() -> Router<AppState> {
  Router::new().route("/v1/config-sync/head", get(head).put(update))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
  expected_head: Option<String>,
  revision_id: String,
  parent_revision_ids: Vec<String>,
  snapshot: Snapshot,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Snapshot {
  schema_version: u32,
  connections: Vec<Selection>,
  active_connection_id: String,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Selection {
  connection_id: String,
  name: String,
  provider_id: String,
  model_id: String,
}

#[derive(Serialize)]
struct Head {
  head: Option<Revision>,
}

#[derive(Serialize)]
struct Revision {
  revision_id: String,
  parent_revision_ids: Vec<String>,
  snapshot: Snapshot,
  updated_at: String,
}

struct Owner {
  issuer: String,
  subject: String,
}

async fn owner(state: &AppState, headers: &HeaderMap, scope: &str) -> Result<Owner> {
  let auth = state.config_sync.as_ref().ok_or_else(|| {
    StoreError::new(
      StatusCode::SERVICE_UNAVAILABLE,
      "config_sync_unavailable",
      "Config sync is unavailable on this Store.",
    )
  })?;
  Ok(Owner {
    issuer: auth.issuer().to_owned(),
    subject: auth.authorize(headers, scope).await?,
  })
}

async fn head(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Head>> {
  let owner = owner(&state, &headers, "config:read").await?;
  query(&state, move |store| store.config_sync_head(&owner))
    .await
    .map(Json)
}

async fn update(State(state): State<AppState>, request: Request) -> Result<Json<Head>> {
  let owner = owner(&state, request.headers(), "config:write").await?;
  let is_json = request
    .headers()
    .get(header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .and_then(|value| value.split(';').next())
    .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
  if !is_json {
    return Err(StoreError::invalid(
      "Send the config snapshot as application/json.",
    ));
  }
  let body = tokio::time::timeout(
    Duration::from_secs(15),
    to_bytes(request.into_body(), MAX_BODY_BYTES),
  )
  .await
  .map_err(|_| StoreError::invalid("The config sync request timed out."))?
  .map_err(|_| StoreError::invalid("Could not read the config snapshot within its size limit."))?;
  let mut write: Write = serde_json::from_slice(&body)
    .map_err(|_| StoreError::invalid("Invalid config sync request."))?;
  write.validate()?;
  query(&state, move |store| {
    store.write_config_sync_head(&owner, write)
  })
  .await
  .map(Json)
}

impl Write {
  fn validate(&mut self) -> Result<()> {
    if !revision_id(&self.revision_id)
      || self
        .expected_head
        .as_ref()
        .is_some_and(|id| !revision_id(id) || id == &self.revision_id)
      || self.parent_revision_ids.as_slice() != self.expected_head.as_slice()
    {
      return Err(StoreError::invalid("Invalid config revision or parent."));
    }
    let mut ids = HashSet::new();
    let snapshot = &mut self.snapshot;
    if snapshot.schema_version != 1
      || snapshot.connections.len() > 31
      || snapshot.connections.iter().any(|entry| {
        entry.connection_id == DEMO_ID
          || !identifier(&entry.connection_id, 128)
          || !ids.insert(entry.connection_id.as_str())
          || !bounded_text(&entry.name, 80)
          || !bounded_text(&entry.model_id, 256)
          || !identifier(&entry.provider_id, 64)
          || matches!(entry.provider_id.as_str(), "custom" | "demo" | "llama-cpp")
      })
      || (snapshot.active_connection_id != DEMO_ID
        && !ids.contains(snapshot.active_connection_id.as_str()))
    {
      return Err(StoreError::invalid(
        "Invalid provider settings in config snapshot.",
      ));
    }
    snapshot
      .connections
      .sort_unstable_by(|a, b| a.connection_id.cmp(&b.connection_id));
    Ok(())
  }
}

fn bounded_text(value: &str, max_bytes: usize) -> bool {
  !value.is_empty()
    && value.len() <= max_bytes
    && value.trim() == value
    && !value.chars().any(char::is_control)
}

fn identifier(value: &str, max_bytes: usize) -> bool {
  !value.is_empty()
    && value.len() <= max_bytes
    && value
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn revision_id(value: &str) -> bool {
  value.len() == 36
    && value.bytes().enumerate().all(|(index, byte)| {
      if matches!(index, 8 | 13 | 18 | 23) {
        byte == b'-'
      } else {
        byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
      }
    })
}

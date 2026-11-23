mod database;
pub(super) mod recovery;
#[cfg(test)]
mod tests;

use super::{
  busy, conflict, missing_pair, now, payload, storage_error, validate_id, Owner, Relay, READ, WRITE,
};
use crate::{Result, StoreError};
use axum::{
  extract::{Path, Request, State},
  http::{HeaderMap, StatusCode},
  routing::{get, post, put},
  Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::{aead, digest, hmac};
use serde::{Deserialize, Serialize};
use std::{fs::File, io::Read, sync::Arc};
use zeroize::{Zeroize, Zeroizing};

pub(super) use database::{has_account, migrate, prune};

pub(super) type Key = Zeroizing<[u8; 32]>;

pub(super) fn master_key_from_env() -> std::result::Result<Option<Key>, String> {
  let Some(file) = std::env::var_os("APELL_ACCOUNT_KEY_EXCHANGE_MASTER_KEY_FILE") else {
    return Ok(None);
  };
  let file =
    File::open(file).map_err(|_| "Could not read the managed key exchange master key file.")?;
  let mut text = Zeroizing::new(String::new());
  file
    .take(128)
    .read_to_string(&mut text)
    .map_err(|_| "Could not read the managed key exchange master key file.")?;
  if text.len() == 128 {
    return Err("Invalid managed key exchange master key file.".into());
  }
  decode_key(text.trim())
    .map(Some)
    .ok_or_else(|| "Invalid managed key exchange master key file.".into())
}

fn master_key(relay: &Relay) -> Result<Arc<Key>> {
  relay.master_key.clone().ok_or_else(|| {
    StoreError::new(
      StatusCode::SERVICE_UNAVAILABLE,
      "managed_key_exchange_unavailable",
      "Managed key exchange is unavailable.",
    )
  })
}

pub(super) fn routes() -> Router<Relay> {
  Router::new()
    .route("/api/v1/key-vault/managed", get(status).put(create))
    .route("/api/v1/key-vault/managed/pairs/list", post(list_pairs))
    .route("/api/v1/key-vault/managed/pairs/{pair_id}", put(start_pair))
    .route(
      "/api/v1/key-vault/managed/pairs/{pair_id}/approve",
      post(approve_pair),
    )
    .route(
      "/api/v1/key-vault/managed/pairs/{pair_id}/finish",
      post(finish_pair),
    )
    .route(
      "/api/v1/key-vault/managed/pairs/{pair_id}/consume",
      post(consume_pair),
    )
    .merge(recovery::routes())
}

#[derive(Serialize)]
struct Status {
  vault_id: Option<String>,
  managed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateVault {
  vault_id: String,
  vault_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreatePair {
  vault_id: String,
  request_secret: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairedProof {
  vault_id: String,
  proof: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairSecret {
  request_secret: String,
}

#[derive(Serialize)]
struct PairSummary {
  pair_id: String,
  code: String,
  approval_nonce: String,
  expires_at: i64,
  approved: bool,
}

#[derive(Serialize)]
struct PairList {
  pairs: Vec<PairSummary>,
}

#[derive(Serialize)]
struct ReceivedKey {
  vault_key: Option<String>,
}

impl Drop for ReceivedKey {
  fn drop(&mut self) {
    self.vault_key.zeroize();
  }
}

fn decode_key(value: &str) -> Option<Key> {
  let mut key = Zeroizing::new([0; 32]);
  if value.len() != 43 || URL_SAFE_NO_PAD.decode_slice(value, key.as_mut()).ok()? != 32 {
    return None;
  }
  Some(key)
}

fn parse_key(value: String) -> Result<Key> {
  decode_key(&Zeroizing::new(value))
    .ok_or_else(|| StoreError::invalid("Invalid managed key exchange secret."))
}

fn request_digest(value: String) -> Result<[u8; 32]> {
  let secret = parse_key(value)?;
  Ok(
    digest::digest(&digest::SHA256, secret.as_ref())
      .as_ref()
      .try_into()
      .expect("SHA-256 output length"),
  )
}

fn random_code() -> Result<String> {
  const SPACE: u32 = 1_000_000;
  loop {
    let mut bytes = [0; 4];
    getrandom::fill(&mut bytes).map_err(|_| storage_error())?;
    let number = u32::from_le_bytes(bytes);
    if number < u32::MAX / SPACE * SPACE {
      return Ok(format!("{:06}", number % SPACE));
    }
  }
}

fn verify_proof(
  key: &Key,
  proof: &[u8],
  operation: &str,
  vault_id: &str,
  pair_id: &str,
  approval_nonce: Option<&str>,
) -> Result<()> {
  let mut input = format!("apell-key-relay-v2\n{operation}\n{vault_id}\n{pair_id}");
  if let Some(approval_nonce) = approval_nonce {
    input.push('\n');
    input.push_str(approval_nonce);
  }
  hmac::verify(
    &hmac::Key::new(hmac::HMAC_SHA256, key.as_ref()),
    input.as_bytes(),
    proof,
  )
  .map_err(|_| {
    StoreError::new(
      StatusCode::FORBIDDEN,
      "paired_device_required",
      "Approve this request on a paired device.",
    )
  })
}

pub(super) async fn load_vault_key(relay: &Relay, owner: &Owner, vault_id: &str) -> Result<Key> {
  let master = master_key(relay)?;
  let owner = owner.clone();
  let vault_id = vault_id.to_owned();
  relay
    .query(move |db| database::load_key(db, &master, &owner, &vault_id))
    .await
}

async fn status(State(relay): State<Relay>, headers: HeaderMap) -> Result<Json<Status>> {
  let owner = relay.owner(&headers, READ).await?;
  master_key(&relay)?;
  relay
    .query(move |db| database::status(db, &owner).map(Json))
    .await
}

async fn create(State(relay): State<Relay>, request: Request) -> Result<StatusCode> {
  let owner = relay.owner(request.headers(), WRITE).await?;
  let master = master_key(&relay)?;
  let input: CreateVault = payload(request).await?;
  validate_id(&input.vault_id)?;
  let key = parse_key(input.vault_key)?;
  relay
    .query(move |db| database::create(db, &master, &owner, &input.vault_id, &key, false))
    .await?;
  Ok(StatusCode::NO_CONTENT)
}

async fn start_pair(
  State(relay): State<Relay>,
  Path(pair_id): Path<String>,
  request: Request,
) -> Result<Json<PairSummary>> {
  let owner = relay.owner(request.headers(), WRITE).await?;
  master_key(&relay)?;
  validate_id(&pair_id)?;
  let input: CreatePair = payload(request).await?;
  validate_id(&input.vault_id)?;
  let digest = request_digest(input.request_secret)?;
  relay
    .query(move |db| {
      database::start_pair(db, &owner, &input.vault_id, &pair_id, &digest, now()).map(Json)
    })
    .await
}

async fn list_pairs(State(relay): State<Relay>, request: Request) -> Result<Json<PairList>> {
  let owner = relay.owner(request.headers(), READ).await?;
  let input: PairedProof = payload(request).await?;
  validate_id(&input.vault_id)?;
  let proof = parse_key(input.proof)?;
  let key = load_vault_key(&relay, &owner, &input.vault_id).await?;
  verify_proof(&key, proof.as_ref(), "list", &input.vault_id, "", None)?;
  relay
    .query(move |db| {
      database::list_pairs(db, &owner, &input.vault_id, now()).map(|pairs| Json(PairList { pairs }))
    })
    .await
}

async fn approve_pair(
  State(relay): State<Relay>,
  Path(pair_id): Path<String>,
  request: Request,
) -> Result<StatusCode> {
  let owner = relay.owner(request.headers(), WRITE).await?;
  let master = master_key(&relay)?;
  validate_id(&pair_id)?;
  let input: PairedProof = payload(request).await?;
  validate_id(&input.vault_id)?;
  let proof = parse_key(input.proof)?;
  relay
    .query(move |db| {
      database::approve_pair(
        db,
        &master,
        &owner,
        &input.vault_id,
        &pair_id,
        &proof,
        now(),
      )
    })
    .await?;
  Ok(StatusCode::NO_CONTENT)
}

async fn finish_pair(
  State(relay): State<Relay>,
  Path(pair_id): Path<String>,
  request: Request,
) -> Result<Json<ReceivedKey>> {
  let owner = relay.owner(request.headers(), READ).await?;
  let master = master_key(&relay)?;
  validate_id(&pair_id)?;
  let input: PairSecret = payload(request).await?;
  let digest = request_digest(input.request_secret)?;
  relay
    .query(move |db| {
      let vault_key = database::finish_pair(db, &master, &owner, &pair_id, &digest, now())?;
      Ok(Json(ReceivedKey {
        vault_key: vault_key.map(|key| URL_SAFE_NO_PAD.encode(key.as_ref())),
      }))
    })
    .await
}

async fn consume_pair(
  State(relay): State<Relay>,
  Path(pair_id): Path<String>,
  request: Request,
) -> Result<StatusCode> {
  let owner = relay.owner(request.headers(), WRITE).await?;
  master_key(&relay)?;
  validate_id(&pair_id)?;
  let input: PairSecret = payload(request).await?;
  let digest = request_digest(input.request_secret)?;
  relay
    .query(move |db| database::consume_pair(db, &owner, &pair_id, &digest, now()))
    .await?;
  Ok(StatusCode::NO_CONTENT)
}

fn escrow_context(owner: &Owner, id: &str) -> Result<Vec<u8>> {
  serde_json::to_vec(&("apell-managed-escrow-v1", &owner.issuer, &owner.subject, id))
    .map_err(|_| storage_error())
}

fn escrow_cipher(master: &Key) -> aead::LessSafeKey {
  aead::LessSafeKey::new(
    aead::UnboundKey::new(&aead::CHACHA20_POLY1305, master.as_ref()).expect("32-byte master key"),
  )
}

fn seal_vault(master: &Key, owner: &Owner, id: &str, key: &Key) -> Result<([u8; 12], Vec<u8>)> {
  let mut nonce = [0; 12];
  getrandom::fill(&mut nonce).map_err(|_| storage_error())?;
  let mut envelope = Zeroizing::new(key.as_ref().to_vec());
  let tag = escrow_cipher(master)
    .seal_in_place_separate_tag(
      aead::Nonce::assume_unique_for_key(nonce),
      aead::Aad::from(escrow_context(owner, id)?),
      &mut envelope,
    )
    .map_err(|_| storage_error())?;
  envelope.extend_from_slice(tag.as_ref());
  Ok((nonce, envelope.to_vec()))
}

fn open_vault(
  master: &Key,
  owner: &Owner,
  id: &str,
  nonce: Vec<u8>,
  envelope: Vec<u8>,
) -> Result<Key> {
  let nonce = aead::Nonce::try_assume_unique_for_key(&nonce).map_err(|_| storage_error())?;
  let mut envelope = Zeroizing::new(envelope);
  let plain = escrow_cipher(master)
    .open_in_place(
      nonce,
      aead::Aad::from(escrow_context(owner, id)?),
      &mut envelope,
    )
    .map_err(|_| storage_error())?;
  let mut key = Zeroizing::new([0; 32]);
  if plain.len() != key.len() {
    return Err(storage_error());
  }
  key.copy_from_slice(plain);
  Ok(key)
}

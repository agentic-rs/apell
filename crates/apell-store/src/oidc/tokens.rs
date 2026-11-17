use super::{
  diagnostics::{emit, reject, Rejection},
  Claims,
};
use crate::{Result, StoreError};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const MAX_ACTIVE: usize = 1024;

#[derive(Clone)]
pub(crate) struct Grant {
  pub applet_id: String,
  pub sha256: String,
  pub expires_at: u64,
}

impl Grant {
  pub fn validate_package(&self, bytes: &[u8], now: u64) -> Result<()> {
    if self.expires_at <= now {
      return Err(reject(Rejection::CredentialExpired));
    }
    if apell_package::digest(bytes) != self.sha256 {
      return Err(reject(Rejection::PackageChecksum));
    }
    let package =
      apell_package::parse(bytes).map_err(|_| StoreError::invalid("Invalid applet package."))?;
    if package.manifest.applet_id != self.applet_id {
      return Err(reject(Rejection::PackageIdentity));
    }
    Ok(())
  }
}

#[derive(Serialize)]
pub(crate) struct TokenResponse {
  pub token: String,
  pub expires_at: u64,
}

#[derive(Default)]
pub(super) struct Tokens {
  grants: HashMap<[u8; 32], Grant>,
  used_ids: HashMap<[u8; 32], u64>,
}

impl Tokens {
  fn prune(&mut self, now: u64) {
    self.grants.retain(|_, grant| grant.expires_at > now);
    self.used_ids.retain(|_, expiry| *expiry > now);
  }

  pub fn mint(
    &mut self,
    claims: &Claims,
    applet_id: String,
    sha256: String,
    now: u64,
  ) -> Result<TokenResponse> {
    self.prune(now);
    let identity = hash(&claims.jti);
    if claims.exp <= now {
      return Err(reject(Rejection::ClaimExpired));
    }
    if self.used_ids.contains_key(&identity) {
      return Err(reject(Rejection::TokenReplayed));
    }
    // Do not evict live replay records or credentials to make room for a request.
    if self.grants.len() >= MAX_ACTIVE || self.used_ids.len() >= MAX_ACTIVE {
      emit(Rejection::CredentialCapacity);
      return Err(StoreError::busy());
    }
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).map_err(|_| StoreError::internal())?;
    let token = format!("apel_ci_{}", apell_package::digest(&random));
    let key = hash(&token);
    if self.grants.contains_key(&key) {
      return Err(StoreError::internal());
    }
    let expires_at = now.saturating_add(300).min(claims.exp);
    self.grants.insert(
      key,
      Grant {
        applet_id,
        sha256,
        expires_at,
      },
    );
    self.used_ids.insert(identity, claims.exp);
    Ok(TokenResponse { token, expires_at })
  }

  pub fn authenticate(&mut self, token: &str, now: u64) -> Result<Grant> {
    self.prune(now);
    self
      .grants
      .get(&hash(token))
      .cloned()
      .ok_or_else(|| reject(Rejection::CredentialUnknown))
  }
}

fn hash(value: &str) -> [u8; 32] {
  Sha256::digest(value.as_bytes()).into()
}

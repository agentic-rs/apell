use super::{unauthorized, unavailable};
use crate::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::DecodingKey;
use serde::Deserialize;
use std::{
  collections::HashMap,
  time::{Duration, Instant},
};
use tokio::sync::Mutex;

const JWKS_URL: &str = "https://token.actions.githubusercontent.com/.well-known/jwks";
const KEY_LIFETIME: Duration = Duration::from_secs(300);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(60);
const MAX_JWKS_BYTES: usize = 256 * 1024;

pub(super) struct Keys {
  client: reqwest::Client,
  endpoint: String,
  cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
  keys: HashMap<String, DecodingKey>,
  fetched_at: Option<Instant>,
  last_attempt: Option<Instant>,
}

#[derive(Deserialize)]
struct Jwks {
  keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
  kty: String,
  alg: String,
  #[serde(rename = "use")]
  usage: String,
  kid: String,
  n: String,
  e: String,
}

impl Keys {
  pub fn new() -> Result<Self> {
    Self::with_endpoint(JWKS_URL, false)
  }

  fn with_endpoint(endpoint: &str, no_proxy: bool) -> Result<Self> {
    let mut builder = reqwest::Client::builder()
      .redirect(reqwest::redirect::Policy::none())
      .connect_timeout(Duration::from_secs(5))
      .read_timeout(Duration::from_secs(5))
      .timeout(Duration::from_secs(10))
      .user_agent("Apell-Store");
    if no_proxy {
      builder = builder.no_proxy();
    }
    Ok(Self {
      client: builder.build().map_err(|_| unavailable())?,
      endpoint: endpoint.into(),
      cache: Mutex::new(Cache::default()),
    })
  }

  #[cfg(test)]
  pub fn fixture(endpoint: &str) -> Self {
    assert!(endpoint.starts_with("http://127.0.0.1:"));
    Self::with_endpoint(endpoint, true).unwrap()
  }

  pub async fn get(&self, kid: &str) -> Result<DecodingKey> {
    // Keep the lock across refresh so concurrent requests cannot fan out to GitHub.
    let mut cache = self.cache.lock().await;
    if cache
      .fetched_at
      .is_some_and(|time| time.elapsed() < KEY_LIFETIME)
    {
      if let Some(key) = cache.keys.get(kid) {
        return Ok(key.clone());
      }
    }
    if cache
      .last_attempt
      .is_some_and(|time| time.elapsed() < REFRESH_COOLDOWN)
    {
      return Err(unauthorized());
    }
    cache.last_attempt = Some(Instant::now());
    let fetched = self.fetch().await?;
    cache.keys = fetched;
    cache.fetched_at = Some(Instant::now());
    cache.keys.get(kid).cloned().ok_or_else(unauthorized)
  }

  async fn fetch(&self) -> Result<HashMap<String, DecodingKey>> {
    let mut response = self
      .client
      .get(&self.endpoint)
      .send()
      .await
      .map_err(|_| unavailable())?;
    if response.status() != reqwest::StatusCode::OK
      || response
        .content_length()
        .is_some_and(|len| len > MAX_JWKS_BYTES as u64)
    {
      return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
      if chunk.len() > MAX_JWKS_BYTES.saturating_sub(bytes.len()) {
        return Err(unavailable());
      }
      bytes.extend_from_slice(&chunk);
    }
    parse_keys(&bytes)
  }

  #[cfg(test)]
  pub async fn expire_for_test(&self) {
    let mut cache = self.cache.lock().await;
    cache.fetched_at = None;
    cache.last_attempt = None;
  }
}

fn parse_keys(bytes: &[u8]) -> Result<HashMap<String, DecodingKey>> {
  let jwks: Jwks = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
  if jwks.keys.is_empty() || jwks.keys.len() > 16 {
    return Err(unavailable());
  }
  let mut keys = HashMap::new();
  for key in jwks.keys {
    let modulus = URL_SAFE_NO_PAD.decode(&key.n).map_err(|_| unavailable())?;
    if key.kty != "RSA"
      || key.alg != "RS256"
      || key.usage != "sig"
      || !valid_kid(&key.kid)
      || !(256..=512).contains(&modulus.len())
      || modulus[0] < 128
      || key.e != "AQAB"
    {
      return Err(unavailable());
    }
    let decoding = DecodingKey::from_rsa_components(&key.n, &key.e).map_err(|_| unavailable())?;
    if keys.insert(key.kid, decoding).is_some() {
      return Err(unavailable());
    }
  }
  Ok(keys)
}

pub(super) fn valid_kid(value: &str) -> bool {
  !value.is_empty() && value.len() <= 128 && value.bytes().all(|c| c.is_ascii_graphic())
}

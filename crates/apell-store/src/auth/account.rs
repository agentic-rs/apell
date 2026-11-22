mod keys;
#[cfg(test)]
mod tests;

use super::{github::valid_login, now};
use crate::{Result, StoreError};
use axum::http::StatusCode;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{Algorithm, Validation};
use reqwest::{header, Client, RequestBuilder};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;
use sha2::{Digest, Sha256, Sha384};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tokio::sync::{Mutex, Semaphore};
use url::Url;

const CACHE_LIFETIME: Duration = Duration::from_secs(300);
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
const MAX_TOKEN_BYTES: usize = 16 * 1024;

pub(super) struct Account {
  issuer: String,
  discovery_url: Url,
  client_id: String,
  client_secret: String,
  redirect_uri: String,
  client: Client,
  discovery: Mutex<Option<(Provider, Instant)>>,
  keys: Mutex<keys::Cache>,
  requests: Semaphore,
}

pub(super) struct AccountIdentity {
  pub issuer: String,
  pub subject: String,
  userinfo_endpoint: Url,
  access_token: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct GitHubIdentity {
  pub id: u64,
  pub login: Option<String>,
}

#[derive(Deserialize)]
struct Discovery {
  issuer: String,
  authorization_endpoint: String,
  token_endpoint: String,
  jwks_uri: String,
  userinfo_endpoint: String,
}

#[derive(Clone)]
struct Provider {
  authorization_endpoint: Url,
  token_endpoint: Url,
  jwks_uri: Url,
  userinfo_endpoint: Url,
}

#[derive(Deserialize)]
struct Tokens {
  token_type: String,
  access_token: String,
  id_token: String,
}

#[derive(Deserialize)]
struct IdClaims {
  sub: String,
  aud: Audience,
  exp: i64,
  iat: i64,
  nonce: String,
  azp: Option<String>,
  at_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
  Single(String),
  Multiple(Vec<String>),
}

impl Audience {
  fn is_for(&self, client_id: &str) -> bool {
    match self {
      Self::Single(audience) => audience == client_id,
      Self::Multiple(audiences) => audiences.len() == 1 && audiences[0] == client_id,
    }
  }
}

#[derive(Deserialize)]
struct UserInfo {
  sub: String,
  identities: Option<Identities>,
}

#[derive(Deserialize)]
struct Identities {
  github: Option<LinkedGitHub>,
}

#[derive(Deserialize)]
struct LinkedGitHub {
  #[serde(rename = "userId")]
  user_id: String,
  details: Option<Value>,
}

impl LinkedGitHub {
  fn verified(self) -> Result<GitHubIdentity> {
    let id = self
      .user_id
      .parse::<u64>()
      .ok()
      .filter(|id| *id > 0 && *id <= i64::MAX as u64 && id.to_string() == self.user_id)
      .ok_or_else(invalid_identity)?;
    // The tagged Logto GitHub connector stores the original GitHub profile in rawData.
    // Its ID must match the linked ID before using that profile's handle.
    let login = self.details.as_ref().and_then(|details| {
      let profile = details.get("rawData")?.get("userInfo")?;
      let login = profile.get("login")?.as_str()?;
      (profile.get("id")?.as_u64()? == id && valid_login(login)).then(|| login.to_owned())
    });
    Ok(GitHubIdentity { id, login })
  }
}

impl Account {
  pub(super) fn new(
    issuer: &str,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
  ) -> Result<Self> {
    let mut discovery_url = issuer_url(issuer).ok_or_else(unavailable)?;
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
      issuer: issuer.to_owned(),
      discovery_url,
      client_id,
      client_secret,
      redirect_uri,
      client: builder.build().map_err(|_| unavailable())?,
      discovery: Mutex::new(None),
      keys: Mutex::new(keys::Cache::default()),
      requests: Semaphore::new(4),
    })
  }

  pub(super) fn security_url(&self) -> Url {
    let mut url = self.discovery_url.clone();
    url.set_path("/account/security");
    url
  }

  pub(super) async fn authorization_url(
    &self,
    state: &str,
    verifier: &str,
    nonce: &str,
  ) -> Result<Url> {
    let _permit = self
      .requests
      .try_acquire()
      .map_err(|_| StoreError::busy())?;
    let mut url = self.provider().await?.authorization_endpoint;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    url.query_pairs_mut().extend_pairs([
      ("response_type", "code"),
      ("client_id", self.client_id.as_str()),
      ("redirect_uri", self.redirect_uri.as_str()),
      ("scope", "openid identities"),
      ("state", state),
      ("nonce", nonce),
      ("code_challenge", challenge.as_str()),
      ("code_challenge_method", "S256"),
    ]);
    Ok(url)
  }

  pub(super) async fn sign_in(
    &self,
    code: &str,
    verifier: &str,
    nonce: &str,
  ) -> Result<AccountIdentity> {
    if !visible_ascii(code, 1024) {
      return Err(invalid_identity());
    }
    let _permit = self
      .requests
      .try_acquire()
      .map_err(|_| StoreError::busy())?;
    let provider = self.provider().await?;
    let body = url::form_urlencoded::Serializer::new(String::new())
      .extend_pairs([
        ("grant_type", "authorization_code"),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", self.redirect_uri.as_str()),
      ])
      .finish();
    let tokens: Tokens = read_json(
      self
        .client
        .post(provider.token_endpoint)
        .basic_auth(
          form_encode(&self.client_id),
          Some(form_encode(&self.client_secret)),
        )
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body),
    )
    .await?;
    if !tokens.token_type.eq_ignore_ascii_case("bearer")
      || !visible_ascii(&tokens.access_token, MAX_TOKEN_BYTES)
      || !visible_ascii(&tokens.id_token, MAX_TOKEN_BYTES)
    {
      return Err(invalid_identity());
    }
    let claims = self
      .verify_identity(&provider.jwks_uri, &tokens, nonce)
      .await?;
    Ok(AccountIdentity {
      issuer: self.issuer.clone(),
      subject: claims.sub,
      userinfo_endpoint: provider.userinfo_endpoint,
      access_token: tokens.access_token,
    })
  }

  pub(super) async fn linked_github(
    &self,
    identity: &AccountIdentity,
  ) -> Result<Option<GitHubIdentity>> {
    let _permit = self
      .requests
      .try_acquire()
      .map_err(|_| StoreError::busy())?;
    let user: UserInfo = read_json(
      self
        .client
        .get(identity.userinfo_endpoint.clone())
        .bearer_auth(&identity.access_token),
    )
    .await?;
    if user.sub != identity.subject {
      return Err(invalid_identity());
    }
    user
      .identities
      .and_then(|identities| identities.github)
      .map(LinkedGitHub::verified)
      .transpose()
  }

  async fn provider(&self) -> Result<Provider> {
    let mut cache = self.discovery.lock().await;
    if let Some((provider, _)) = cache
      .as_ref()
      .filter(|(_, time)| time.elapsed() < CACHE_LIFETIME)
    {
      return Ok(provider.clone());
    }
    let document: Discovery = read_json(self.client.get(self.discovery_url.clone())).await?;
    if document.issuer != self.issuer {
      return Err(unavailable());
    }
    let issuer = issuer_url(&self.issuer).ok_or_else(unavailable)?;
    let prefix = format!("{}/", issuer.path().trim_end_matches('/'));
    let endpoint = |value: &str| {
      parse_url(value)
        .filter(|url| url.origin() == issuer.origin() && url.path().starts_with(&prefix))
        .ok_or_else(unavailable)
    };
    let provider = Provider {
      authorization_endpoint: endpoint(&document.authorization_endpoint)?,
      token_endpoint: endpoint(&document.token_endpoint)?,
      jwks_uri: endpoint(&document.jwks_uri)?,
      userinfo_endpoint: endpoint(&document.userinfo_endpoint)?,
    };
    *cache = Some((provider.clone(), Instant::now()));
    Ok(provider)
  }

  async fn verify_identity(
    &self,
    jwks_uri: &Url,
    tokens: &Tokens,
    nonce: &str,
  ) -> Result<IdClaims> {
    let header = jsonwebtoken::decode_header(&tokens.id_token).map_err(|_| invalid_identity())?;
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
      || header
        .typ
        .as_deref()
        .is_some_and(|value| !value.eq_ignore_ascii_case("jwt"))
    {
      return Err(invalid_identity());
    }
    let kid = header
      .kid
      .as_deref()
      .filter(|kid| visible_ascii(kid, 128))
      .ok_or_else(invalid_identity)?;
    let key = {
      let mut keys = self.keys.lock().await;
      keys.get(&self.client, jwks_uri, kid, header.alg).await?
    };
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[&self.issuer]);
    validation.set_audience(&[&self.client_id]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation.validate_nbf = true;
    validation.leeway = 0;
    let claims = jsonwebtoken::decode::<IdClaims>(&tokens.id_token, &key, &validation)
      .map_err(|_| invalid_identity())?
      .claims;
    let now = now();
    if !visible_ascii(&claims.sub, 255)
      || !claims.aud.is_for(&self.client_id)
      || claims
        .azp
        .as_ref()
        .is_some_and(|party| party != &self.client_id)
      || claims.exp <= now
      || claims.iat <= 0
      || claims.iat > now.saturating_add(60)
      || claims.iat >= claims.exp
      || !constant_time_equal(&claims.nonce, nonce)
      || claims.at_hash.as_ref().is_some_and(|hash| {
        !constant_time_equal(hash, &access_token_hash(header.alg, &tokens.access_token))
      })
    {
      return Err(invalid_identity());
    }
    Ok(claims)
  }
}

pub(super) fn validate_issuer(issuer: &str) -> bool {
  issuer_url(issuer).is_some()
}

fn issuer_url(value: &str) -> Option<Url> {
  let url = parse_url(value)?;
  (url.path() == "/" || !url.path().ends_with('/')).then_some(url)
}

fn parse_url(value: &str) -> Option<Url> {
  if value.len() > 2048
    || value
      .chars()
      .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    || value.contains(['?', '#'])
  {
    return None;
  }
  let (_, authority_and_path) = value.split_once("://")?;
  let authority = authority_and_path.split('/').next()?;
  if authority.is_empty() || authority.contains('@') {
    return None;
  }
  let url = Url::parse(value).ok()?;
  let allowed_scheme = url.scheme() == "https"
    || (cfg!(test) && url.scheme() == "http" && url.host_str() == Some("127.0.0.1"));
  let canonical =
    url.as_str() == value || (url.path() == "/" && url.as_str().strip_suffix('/') == Some(value));
  (allowed_scheme && canonical && url.has_host()).then_some(url)
}

fn form_encode(value: &str) -> String {
  url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

fn visible_ascii(value: &str, max_len: usize) -> bool {
  !value.is_empty() && value.len() <= max_len && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn constant_time_equal(first: &str, second: &str) -> bool {
  bool::from(first.as_bytes().ct_eq(second.as_bytes()))
}

fn access_token_hash(algorithm: Algorithm, token: &str) -> String {
  if algorithm == Algorithm::ES384 {
    URL_SAFE_NO_PAD.encode(&Sha384::digest(token.as_bytes())[..24])
  } else {
    URL_SAFE_NO_PAD.encode(&Sha256::digest(token.as_bytes())[..16])
  }
}

async fn read_json<T: DeserializeOwned>(request: RequestBuilder) -> Result<T> {
  let mut response = request
    .header(header::ACCEPT, "application/json")
    .send()
    .await
    .map_err(|_| unavailable())?;
  if response.status() != StatusCode::OK
    || response
      .content_length()
      .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
  {
    return Err(unavailable());
  }
  let mut bytes = Vec::new();
  while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
    if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
      return Err(unavailable());
    }
    bytes.extend_from_slice(&chunk);
  }
  serde_json::from_slice(&bytes).map_err(|_| unavailable())
}

fn unavailable() -> StoreError {
  StoreError::new(
    StatusCode::BAD_GATEWAY,
    "account_unavailable",
    "Account sign-in is temporarily unavailable. Please try again.",
  )
}

fn invalid_identity() -> StoreError {
  StoreError::new(
    StatusCode::UNAUTHORIZED,
    "invalid_account_identity",
    "Account sign-in could not be verified. Please try again.",
  )
}

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{jwk::JwkSet, Algorithm, DecodingKey, Validation};
use reqwest::{header, Client, RequestBuilder};
use serde::{de::DeserializeOwned, Deserialize};
use sha2::{Digest, Sha256, Sha384};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::Url;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use zeroize::Zeroizing;

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const MIN_POLL_INTERVAL: u64 = 5;
const MAX_CHALLENGE_LIFETIME: u64 = 15 * 60;
const MAX_REPLY: usize = 256 * 1024;
const INVALID_IDENTITY: &str = "Apell Account returned an invalid identity.";
const UNAVAILABLE: &str = "Could not reach Apell Account. Try again.";
pub(super) const SIGN_IN_EXPIRED: &str = "Apell Account sign-in expired. Try again.";

pub(super) struct Config {
  pub issuer: Url,
  pub client_id: &'static str,
  pub sync_url: Url,
  pub resource: &'static str,
  pub key_exchange: Option<Url>,
}

impl Config {
  pub fn compiled() -> Option<Self> {
    if !cfg!(any(
      target_os = "ios",
      target_os = "macos",
      target_os = "android"
    )) {
      return None;
    }
    let issuer = secure_url(
      option_env!("APELL_ACCOUNT_ISSUER").or(option_env!("TAURI_APELL_ACCOUNT_ISSUER"))?,
    )?;
    let client_id =
      option_env!("APELL_ACCOUNT_CLIENT_ID").or(option_env!("TAURI_APELL_ACCOUNT_CLIENT_ID"))?;
    let sync_url =
      secure_url(option_env!("APELL_SYNC_URL").or(option_env!("TAURI_APELL_SYNC_URL"))?)?;
    let resource =
      option_env!("APELL_SYNC_RESOURCE").or(option_env!("TAURI_APELL_SYNC_RESOURCE"))?;
    secure_url(resource)?;
    let key_exchange = option_env!("APELL_KEY_EXCHANGE_RESOURCE")
      .or(option_env!("TAURI_APELL_KEY_EXCHANGE_RESOURCE"))
      .and_then(secure_url)
      .filter(|url| url.origin() == issuer.origin() && url.as_str() != resource);
    if !is_identifier(client_id, 128) || sync_url.path() != "/" {
      return None;
    }
    Some(Self {
      issuer,
      client_id,
      sync_url,
      resource,
      key_exchange,
    })
  }
}

fn secure_url(value: &str) -> Option<Url> {
  let url = Url::parse(value).ok()?;
  let authority = value.strip_prefix("https://")?.split('/').next()?;
  if value.len() > 2048
    || authority.contains('@')
    || !url.has_host()
    || url.query().is_some()
    || url.fragment().is_some()
    || !(url.as_str() == value
      || url.path() == "/" && url.as_str().strip_suffix('/') == Some(value))
  {
    return None;
  }
  Some(url)
}

#[derive(Deserialize)]
struct Discovery {
  issuer: String,
  device_authorization_endpoint: Option<String>,
  token_endpoint: String,
  jwks_uri: String,
  revocation_endpoint: String,
}

pub(super) struct Provider {
  device_authorization: Url,
  token: Url,
  jwks: Url,
  revocation: Url,
}

pub(super) async fn discover(client: &Client, config: &Config) -> Result<Provider, String> {
  let discovery = format!(
    "{}/.well-known/openid-configuration",
    config.issuer.as_str().trim_end_matches('/')
  );
  let doc: Discovery = read(client.get(discovery)).await?;
  if doc.issuer != config.issuer.as_str() {
    return Err(UNAVAILABLE.into());
  }
  let path_prefix = format!("{}/", config.issuer.path().trim_end_matches('/'));
  let endpoint = |value: &str| {
    secure_url(value)
      .filter(|url| url.origin() == config.issuer.origin() && url.path().starts_with(&path_prefix))
      .ok_or_else(|| UNAVAILABLE.to_owned())
  };
  Ok(Provider {
    device_authorization: endpoint(
      doc
        .device_authorization_endpoint
        .as_deref()
        .ok_or("Apell Account device sign-in is unavailable.")?,
    )?,
    token: endpoint(&doc.token_endpoint)?,
    jwks: endpoint(&doc.jwks_uri)?,
    revocation: endpoint(&doc.revocation_endpoint)?,
  })
}

#[derive(Deserialize)]
struct DeviceResponse {
  device_code: String,
  user_code: String,
  verification_uri: String,
  verification_uri_complete: Option<String>,
  expires_in: u64,
  interval: Option<u64>,
}

#[derive(Clone, serde::Serialize)]
pub(super) struct DeviceCode {
  pub user_code: String,
  pub verification_uri: Url,
  pub expires_at: String,
}

pub(super) struct Challenge {
  pub display: DeviceCode,
  pub browser_url: Url,
  pub deadline: tokio::time::Instant,
  pub requested_at: u64,
  secret: Zeroizing<String>,
  interval: Duration,
}

pub(super) async fn start_device_sign_in(
  client: &Client,
  config: &Config,
  provider: &Provider,
) -> Result<Challenge, String> {
  let requested = OffsetDateTime::now_utc();
  let mut form = vec![
    ("client_id", config.client_id),
    ("resource", config.resource),
  ];
  let scope = if let Some(resource) = &config.key_exchange {
    form.push(("resource", resource.as_str()));
    "openid profile email offline_access config:read config:write credentials:read credentials:write key_exchange:read key_exchange:write"
  } else {
    "openid profile email offline_access config:read config:write"
  };
  form.push(("scope", scope));
  let request = post_form(client, provider.device_authorization.clone(), &form);
  let response: DeviceResponse = read(request).await?;
  challenge(config, response, requested).ok_or_else(|| UNAVAILABLE.into())
}

fn challenge(
  config: &Config,
  response: DeviceResponse,
  requested: OffsetDateTime,
) -> Option<Challenge> {
  let secret = Zeroizing::new(response.device_code);
  if !valid_token(&secret) || !is_identifier(&response.user_code, 32) || response.expires_in == 0 {
    return None;
  }
  let verification_uri =
    secure_url(&response.verification_uri).filter(|url| url.origin() == config.issuer.origin())?;
  let browser_url = response
    .verification_uri_complete
    .as_deref()
    .and_then(|address| {
      let (base, _) = address.split_once('?')?;
      if base != verification_uri.as_str() || address.len() > 2048 {
        return None;
      }
      let url = Url::parse(address).ok()?;
      let mut query = url.query_pairs();
      let (name, value) = query.next()?;
      (url.fragment().is_none()
        && name == "user_code"
        && value == response.user_code
        && query.next().is_none())
      .then_some(url)
    })
    .unwrap_or_else(|| verification_uri.clone());
  let lifetime = Duration::from_secs(response.expires_in.min(MAX_CHALLENGE_LIFETIME));
  let expires_at = (OffsetDateTime::now_utc() + lifetime)
    .format(&Rfc3339)
    .ok()?;
  Some(Challenge {
    display: DeviceCode {
      user_code: response.user_code,
      verification_uri,
      expires_at,
    },
    browser_url,
    deadline: tokio::time::Instant::now() + lifetime,
    requested_at: requested.unix_timestamp().try_into().ok()?,
    secret,
    interval: Duration::from_secs(
      response
        .interval
        .unwrap_or(MIN_POLL_INTERVAL)
        .clamp(MIN_POLL_INTERVAL, MAX_CHALLENGE_LIFETIME),
    ),
  })
}

#[derive(Deserialize)]
pub(super) struct Tokens {
  pub access_token: String,
  pub refresh_token: Option<String>,
  pub id_token: Option<String>,
  pub expires_in: u64,
  token_type: String,
}

#[derive(Deserialize)]
struct OAuthError {
  error: String,
}

pub(super) enum TokenError {
  Expired,
  Failed(String),
}

impl TokenError {
  pub fn message(self) -> String {
    match self {
      Self::Expired => "Your Apell Account session expired. Sign in again.".into(),
      Self::Failed(message) => message,
    }
  }
}

enum Poll {
  Pending,
  SlowDown,
  Retry,
  Authorized(Tokens),
}

pub(super) async fn await_device_approval(
  client: &Client,
  config: &Config,
  provider: &Provider,
  challenge: &Challenge,
) -> Result<Tokens, String> {
  tokio::time::timeout_at(challenge.deadline, async {
    let mut interval = challenge.interval;
    loop {
      tokio::time::sleep(interval).await;
      match poll_device_code(client, config, provider, &challenge.secret).await? {
        Poll::Pending => {}
        Poll::SlowDown => interval += Duration::from_secs(MIN_POLL_INTERVAL),
        Poll::Retry => interval *= 2,
        Poll::Authorized(tokens) => return Ok(tokens),
      }
    }
  })
  .await
  .map_err(|_| SIGN_IN_EXPIRED.to_owned())?
}

async fn poll_device_code(
  client: &Client,
  config: &Config,
  provider: &Provider,
  device_code: &str,
) -> Result<Poll, String> {
  let response = post_form(
    client,
    provider.token.clone(),
    &[
      ("grant_type", DEVICE_GRANT),
      ("client_id", config.client_id),
      ("resource", config.resource),
      ("device_code", device_code),
    ],
  )
  .send()
  .await;
  let Ok(response) = response else {
    return Ok(Poll::Retry);
  };
  let status = response.status();
  if status.is_server_error() {
    return Ok(Poll::Retry);
  }
  if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
    return Ok(Poll::SlowDown);
  }
  let body = body(response).await?;
  if status.is_success() {
    return parse_tokens(&body)
      .map(Poll::Authorized)
      .map_err(TokenError::message);
  }
  if status == reqwest::StatusCode::BAD_REQUEST {
    return device_poll_error(&body);
  }
  Err(UNAVAILABLE.into())
}

fn device_poll_error(body: &[u8]) -> Result<Poll, String> {
  let error: OAuthError = serde_json::from_slice(body).map_err(|_| UNAVAILABLE)?;
  match error.error.as_str() {
    "authorization_pending" => Ok(Poll::Pending),
    "slow_down" => Ok(Poll::SlowDown),
    "expired_token" => Err(SIGN_IN_EXPIRED.into()),
    "access_denied" => Err("Apell Account sign-in was declined. Try again.".into()),
    _ => Err(UNAVAILABLE.into()),
  }
}

pub(super) async fn refresh(
  client: &Client,
  config: &Config,
  provider: &Provider,
  refresh_token: &str,
  resource: &str,
) -> Result<Tokens, TokenError> {
  token(
    client,
    provider,
    &[
      ("grant_type", "refresh_token"),
      ("client_id", config.client_id),
      ("resource", resource),
      ("refresh_token", refresh_token),
    ],
  )
  .await
}

pub(super) async fn revoke(
  client: &Client,
  config: &Config,
  provider: &Provider,
  refresh_token: &str,
) {
  let _ = post_form(
    client,
    provider.revocation.clone(),
    &[
      ("client_id", config.client_id),
      ("token", refresh_token),
      ("token_type_hint", "refresh_token"),
    ],
  )
  .send()
  .await;
}

async fn token(
  client: &Client,
  provider: &Provider,
  form: &[(&str, &str)],
) -> Result<Tokens, TokenError> {
  let failure = || TokenError::Failed(UNAVAILABLE.into());
  let response = post_form(client, provider.token.clone(), form)
    .send()
    .await
    .map_err(|_| failure())?;
  let status = response.status();
  let body = body(response).await.map_err(TokenError::Failed)?;
  if !status.is_success() {
    if status == reqwest::StatusCode::BAD_REQUEST
      && serde_json::from_slice::<OAuthError>(&body)
        .is_ok_and(|value| value.error == "invalid_grant")
    {
      return Err(TokenError::Expired);
    }
    return Err(failure());
  }
  parse_tokens(&body)
}

fn parse_tokens(body: &[u8]) -> Result<Tokens, TokenError> {
  let tokens: Tokens =
    serde_json::from_slice(body).map_err(|_| TokenError::Failed(UNAVAILABLE.into()))?;
  if !tokens.token_type.eq_ignore_ascii_case("bearer")
    || tokens.expires_in == 0
    || !valid_token(&tokens.access_token)
    || tokens
      .refresh_token
      .as_deref()
      .is_some_and(|value| !valid_token(value))
    || tokens
      .id_token
      .as_deref()
      .is_some_and(|value| !valid_token(value))
  {
    return Err(TokenError::Failed(INVALID_IDENTITY.into()));
  }
  Ok(tokens)
}

fn post_form(client: &Client, url: Url, form: &[(&str, &str)]) -> RequestBuilder {
  let body = url::form_urlencoded::Serializer::new(String::new())
    .extend_pairs(form.iter().copied())
    .finish();
  client
    .post(url)
    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    .body(body)
}

#[derive(Deserialize)]
pub(super) struct Identity {
  pub sub: String,
  pub email: Option<String>,
  pub email_verified: Option<bool>,
  iat: u64,
  exp: u64,
  azp: Option<String>,
  aud: Audience,
  at_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
  Single(String),
  Multiple(Vec<String>),
}

pub(super) async fn verify(
  client: &Client,
  config: &Config,
  provider: &Provider,
  tokens: &Tokens,
  requested_at: u64,
) -> Result<Identity, String> {
  let jwks = read(client.get(provider.jwks.clone())).await?;
  verify_with_keys(config, tokens, requested_at, &jwks)
}

fn verify_with_keys(
  config: &Config,
  tokens: &Tokens,
  requested_at: u64,
  jwks: &JwkSet,
) -> Result<Identity, String> {
  let invalid = || INVALID_IDENTITY.to_string();
  let id_token = tokens.id_token.as_deref().ok_or_else(invalid)?;
  let header = jsonwebtoken::decode_header(id_token).map_err(|_| invalid())?;
  let kid = header
    .kid
    .as_deref()
    .filter(|value| visible_ascii(value, 128))
    .ok_or_else(invalid)?;
  if !matches!(
    header.alg,
    Algorithm::ES384 | Algorithm::ES256 | Algorithm::RS256
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
    return Err(invalid());
  }
  let mut candidates = jwks
    .keys
    .iter()
    .filter(|key| key.common.key_id.as_deref() == Some(kid));
  let jwk = candidates.next().ok_or_else(invalid)?;
  if candidates.next().is_some() || !key_matches(jwk, header.alg) {
    return Err(invalid());
  }
  let key = DecodingKey::from_jwk(jwk).map_err(|_| invalid())?;
  let mut validation = Validation::new(header.alg);
  validation.set_issuer(&[config.issuer.as_str()]);
  validation.set_audience(&[config.client_id]);
  validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
  validation.validate_nbf = true;
  validation.leeway = 0;
  let claims = jsonwebtoken::decode::<Identity>(id_token, &key, &validation)
    .map_err(|_| invalid())?
    .claims;
  let now = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_err(|_| invalid())?
    .as_secs();
  let audience_matches = match &claims.aud {
    Audience::Single(value) => value == config.client_id,
    Audience::Multiple(values) => values.as_slice() == [config.client_id],
  };
  if !visible_ascii(&claims.sub, 255)
    || !audience_matches
    || claims.iat == 0
    || claims.iat < requested_at.saturating_sub(60)
    || claims.iat > now.saturating_add(60)
    || claims.exp <= now
    || claims.iat >= claims.exp
    || claims
      .azp
      .as_deref()
      .is_some_and(|value| value != config.client_id)
    || claims
      .at_hash
      .as_deref()
      .is_some_and(|value| value != access_hash(header.alg, &tokens.access_token))
  {
    return Err(invalid());
  }
  Ok(claims)
}

fn key_matches(jwk: &jsonwebtoken::jwk::Jwk, algorithm: Algorithm) -> bool {
  use jsonwebtoken::jwk::{
    AlgorithmParameters, EllipticCurve, KeyAlgorithm, KeyOperations, PublicKeyUse,
  };
  if jwk
    .common
    .public_key_use
    .as_ref()
    .is_some_and(|usage| *usage != PublicKeyUse::Signature)
    || jwk
      .common
      .key_operations
      .as_ref()
      .is_some_and(|ops| !ops.contains(&KeyOperations::Verify))
  {
    return false;
  }
  match (&jwk.algorithm, algorithm, jwk.common.key_algorithm) {
    (AlgorithmParameters::EllipticCurve(ec), Algorithm::ES384, Some(KeyAlgorithm::ES384)) => {
      ec.curve == EllipticCurve::P384
    }
    (AlgorithmParameters::EllipticCurve(ec), Algorithm::ES256, Some(KeyAlgorithm::ES256)) => {
      ec.curve == EllipticCurve::P256
    }
    (AlgorithmParameters::RSA(rsa), Algorithm::RS256, Some(KeyAlgorithm::RS256)) => {
      rsa.e == "AQAB"
        && URL_SAFE_NO_PAD
          .decode(&rsa.n)
          .is_ok_and(|n| (256..=512).contains(&n.len()))
    }
    _ => false,
  }
}

fn access_hash(algorithm: Algorithm, token: &str) -> String {
  if algorithm == Algorithm::ES384 {
    URL_SAFE_NO_PAD.encode(&Sha384::digest(token.as_bytes())[..24])
  } else {
    URL_SAFE_NO_PAD.encode(&Sha256::digest(token.as_bytes())[..16])
  }
}

async fn read<T: DeserializeOwned>(request: RequestBuilder) -> Result<T, String> {
  let response = request.send().await.map_err(|_| UNAVAILABLE)?;
  if !response.status().is_success() {
    return Err(UNAVAILABLE.into());
  }
  serde_json::from_slice(&body(response).await?).map_err(|_| UNAVAILABLE.into())
}

async fn body(mut response: reqwest::Response) -> Result<Zeroizing<Vec<u8>>, String> {
  let mut result = Zeroizing::new(Vec::new());
  if response
    .content_length()
    .is_some_and(|size| size > MAX_REPLY as u64)
  {
    return Err(UNAVAILABLE.into());
  }
  while let Some(chunk) = response.chunk().await.map_err(|_| UNAVAILABLE)? {
    if result.len().saturating_add(chunk.len()) > MAX_REPLY {
      return Err(UNAVAILABLE.into());
    }
    result.extend_from_slice(&chunk);
  }
  Ok(result)
}

pub(super) fn visible_ascii(value: &str, max: usize) -> bool {
  !value.is_empty() && value.len() <= max && value.bytes().all(|byte| byte.is_ascii_graphic())
}

pub(super) fn valid_token(value: &str) -> bool {
  visible_ascii(value, 16 * 1024)
}

fn is_identifier(value: &str, max: usize) -> bool {
  visible_ascii(value, max)
    && value
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
  use super::*;
  use jsonwebtoken::{encode, jwk::Jwk, EncodingKey, Header};
  use serde_json::json;

  fn fixture() -> Config {
    Config {
      issuer: Url::parse("https://account.example.invalid/oidc").unwrap(),
      client_id: "fixture-native",
      sync_url: Url::parse("https://sync.example.invalid/").unwrap(),
      resource: "https://sync.example.invalid/config-sync",
      key_exchange: None,
    }
  }

  fn device_response(verification_uri: &str, complete: &str) -> DeviceResponse {
    DeviceResponse {
      device_code: "private-poll-secret".into(),
      user_code: "A2BC-DEF3".into(),
      verification_uri: verification_uri.into(),
      verification_uri_complete: Some(complete.into()),
      expires_in: 300,
      interval: None,
    }
  }

  #[test]
  fn device_code_display_omits_the_secret_and_prefilled_url() {
    let config = fixture();
    let base = "https://account.example.invalid/device";
    let complete = "https://account.example.invalid/device?user_code=A2BC-DEF3";
    let attempt = challenge(
      &config,
      device_response(base, complete),
      OffsetDateTime::now_utc(),
    )
    .unwrap();
    assert_eq!(attempt.browser_url.as_str(), complete);
    assert_eq!(attempt.interval, Duration::from_secs(5));
    let display = serde_json::to_value(attempt.display).unwrap();
    assert_eq!(display["verification_uri"], base);
    assert_eq!(display["user_code"], "A2BC-DEF3");
    assert_eq!(display.as_object().unwrap().len(), 3);
  }

  #[test]
  fn verification_rejects_foreign_origins_and_ignores_invalid_complete_urls() {
    let config = fixture();
    let base = "https://account.example.invalid/device";
    let now = OffsetDateTime::now_utc();
    assert!(challenge(
      &config,
      device_response("https://other.example.invalid/device", ""),
      now
    )
    .is_none());
    for address in [
      "https://other.example.invalid/device?user_code=A2BC-DEF3",
      "https://account.example.invalid/elsewhere?user_code=A2BC-DEF3",
      "https://account.example.invalid/device?user_code=WRONG",
      "https://account.example.invalid/device?user_code=A2BC-DEF3&next=https://other.example.invalid",
    ] {
      let attempt = challenge(&config, device_response(base, address), now).unwrap();
      assert_eq!(attempt.browser_url.as_str(), base);
    }
  }

  #[test]
  fn device_polling_distinguishes_retryable_and_terminal_responses() {
    let poll =
      |error: &str| device_poll_error(&serde_json::to_vec(&json!({"error":error})).unwrap());
    assert!(matches!(poll("authorization_pending"), Ok(Poll::Pending)));
    assert!(matches!(poll("slow_down"), Ok(Poll::SlowDown)));
    assert_eq!(
      poll("expired_token").err().as_deref(),
      Some(SIGN_IN_EXPIRED)
    );
    assert!(poll("access_denied").is_err());
    assert!(poll("invalid_grant").is_err());
  }

  #[test]
  fn signed_es384_identity_binds_freshness_audience_and_access_token() {
    let config = fixture();
    // PKCS#8 wrapping a freshly generated SEC1 P-384 test key.
    let mut der = vec![
      0x30, 0x4e, 0x02, 0x01, 0x00, 0x30, 0x10, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02,
      0x01, 0x06, 0x05, 0x2b, 0x81, 0x04, 0x00, 0x22, 0x04, 0x37, 0x30, 0x35, 0x02, 0x01, 0x01,
      0x04, 0x30,
    ];
    let mut scalar = [0; 48];
    getrandom::fill(&mut scalar).unwrap();
    scalar[0] = 1;
    der.extend_from_slice(&scalar);
    let signer = EncodingKey::from_ec_der(&der);
    let mut jwk = Jwk::from_encoding_key(&signer, Algorithm::ES384).unwrap();
    jwk.common.key_id = Some("fixture-key".into());
    let jwks = JwkSet { keys: vec![jwk] };
    let mut header = Header::new(Algorithm::ES384);
    header.kid = Some("fixture-key".into());
    let now = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap()
      .as_secs();
    let access_token = "fixture-access";
    let claims = json!({
      "iss": config.issuer.as_str(), "aud": config.client_id, "sub": "signed-subject",
      "iat": now, "exp": now + 60,
      "at_hash": access_hash(header.alg, access_token)
    });
    let mut tokens = Tokens {
      access_token: access_token.into(),
      refresh_token: None,
      id_token: Some(encode(&header, &claims, &signer).unwrap()),
      expires_in: 60,
      token_type: "Bearer".into(),
    };
    assert_eq!(
      verify_with_keys(&config, &tokens, now, &jwks).unwrap().sub,
      "signed-subject"
    );
    assert!(verify_with_keys(&config, &tokens, now + 61, &jwks).is_err());
    tokens.access_token = "other-access".into();
    assert!(verify_with_keys(&config, &tokens, now, &jwks).is_err());
    tokens.access_token = access_token.into();
    let mut wrong_audience = claims;
    wrong_audience["aud"] = json!("other-client");
    tokens.id_token = Some(encode(&header, &wrong_audience, &signer).unwrap());
    assert!(verify_with_keys(&config, &tokens, now, &jwks).is_err());
  }
}

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{jwk::JwkSet, Algorithm, DecodingKey, Validation};
use reqwest::{header, Client, RequestBuilder};
use serde::{de::DeserializeOwned, Deserialize};
use sha2::{Digest, Sha256, Sha384};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Url;
use zeroize::Zeroizing;

pub(super) const REDIRECT_URI: &str = "dev.apell.shell://oauth/callback";
const MAX_REPLY: usize = 256 * 1024;
const INVALID_IDENTITY: &str = "Apell Account returned an invalid identity.";
const UNAVAILABLE: &str = "Could not reach Apell Account. Try again.";

pub(super) struct Config {
  pub issuer: Url,
  pub client_id: &'static str,
  pub sync_url: Url,
  pub resource: &'static str,
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
    let issuer = secure_url(option_env!("APELL_ACCOUNT_ISSUER")?)?;
    let client_id = option_env!("APELL_ACCOUNT_CLIENT_ID")?;
    let sync_url = secure_url(option_env!("APELL_SYNC_URL")?)?;
    let resource = option_env!("APELL_SYNC_RESOURCE")?;
    secure_url(resource)?;
    if !is_identifier(client_id, 128) || sync_url.path() != "/" {
      return None;
    }
    Some(Self {
      issuer,
      client_id,
      sync_url,
      resource,
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
  authorization_endpoint: String,
  token_endpoint: String,
  jwks_uri: String,
  revocation_endpoint: String,
  code_challenge_methods_supported: Vec<String>,
}

#[derive(Clone)]
pub(super) struct Provider {
  authorization: Url,
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
  if doc.issuer != config.issuer.as_str()
    || !doc
      .code_challenge_methods_supported
      .iter()
      .any(|method| method == "S256")
  {
    return Err(UNAVAILABLE.into());
  }
  let path_prefix = format!("{}/", config.issuer.path().trim_end_matches('/'));
  let endpoint = |value: &str| {
    secure_url(value)
      .filter(|url| url.origin() == config.issuer.origin() && url.path().starts_with(&path_prefix))
      .ok_or_else(|| UNAVAILABLE.to_owned())
  };
  Ok(Provider {
    authorization: endpoint(&doc.authorization_endpoint)?,
    token: endpoint(&doc.token_endpoint)?,
    jwks: endpoint(&doc.jwks_uri)?,
    revocation: endpoint(&doc.revocation_endpoint)?,
  })
}

pub(super) fn authorize(
  config: &Config,
  provider: &Provider,
  state: &str,
  nonce: &str,
  verifier: &str,
) -> Url {
  let mut url = provider.authorization.clone();
  url.query_pairs_mut().extend_pairs([
    ("client_id", config.client_id),
    ("redirect_uri", REDIRECT_URI),
    ("response_type", "code"),
    (
      "code_challenge",
      &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
    ),
    ("code_challenge_method", "S256"),
    ("state", state),
    ("nonce", nonce),
    (
      "scope",
      "openid profile email offline_access config:read config:write",
    ),
    ("resource", config.resource),
    ("prompt", "consent"),
  ]);
  url
}

pub(super) struct Callback<'a> {
  pub state: std::borrow::Cow<'a, str>,
  pub issuer: std::borrow::Cow<'a, str>,
  pub code: Option<Zeroizing<String>>,
}

pub(super) fn callback(url: &Url) -> Option<Callback<'_>> {
  if url.as_str().len() > 4096 || url.fragment().is_some() {
    return None;
  }
  let mut target = url.clone();
  target.set_query(None);
  if target.as_str() != REDIRECT_URI {
    return None;
  }
  let (mut state, mut issuer, mut code, mut error) = (None, None, None, None);
  for (key, value) in url.query_pairs() {
    let slot = match key.as_ref() {
      "state" => &mut state,
      "iss" => &mut issuer,
      "code" => &mut code,
      "error" => &mut error,
      _ => continue,
    };
    if slot.replace(value).is_some() {
      return None;
    }
  }
  let code = match (code, error) {
    (Some(code), None) if visible_ascii(&code, 1024) => Some(Zeroizing::new(code.into_owned())),
    (None, Some(_)) => None,
    _ => return None,
  };
  Some(Callback {
    state: state?,
    issuer: issuer?,
    code,
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

pub(super) async fn exchange(
  client: &Client,
  config: &Config,
  provider: &Provider,
  code: &str,
  verifier: &str,
) -> Result<Tokens, TokenError> {
  token(
    client,
    provider,
    &[
      ("grant_type", "authorization_code"),
      ("client_id", config.client_id),
      ("redirect_uri", REDIRECT_URI),
      ("resource", config.resource),
      ("code", code),
      ("code_verifier", verifier),
    ],
  )
  .await
}

pub(super) async fn refresh(
  client: &Client,
  config: &Config,
  provider: &Provider,
  refresh_token: &str,
) -> Result<Tokens, TokenError> {
  token(
    client,
    provider,
    &[
      ("grant_type", "refresh_token"),
      ("client_id", config.client_id),
      ("resource", config.resource),
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
  let tokens: Tokens = serde_json::from_slice(&body).map_err(|_| failure())?;
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
  nonce: String,
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
  nonce: &str,
) -> Result<Identity, String> {
  let jwks = read(client.get(provider.jwks.clone())).await?;
  verify_with_keys(config, tokens, nonce, &jwks)
}

fn verify_with_keys(
  config: &Config,
  tokens: &Tokens,
  nonce: &str,
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
    || claims.nonce != nonce
    || claims.iat == 0
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

  #[test]
  fn callback_requires_exact_target_and_unambiguous_security_parameters() {
    let url = |value| Url::parse(value).unwrap();
    let valid = url("dev.apell.shell://oauth/callback?iss=https%3A%2F%2Faccount.example.invalid%2Foidc&state=once&code=code");
    let parsed = callback(&valid).unwrap();
    assert_eq!(parsed.issuer, "https://account.example.invalid/oidc");
    assert_eq!(parsed.state, "once");
    assert_eq!(
      parsed.code.as_ref().map(|value| value.as_str()),
      Some("code")
    );
    for wrong in [
      "dev.apell.shell://oauth/elsewhere?iss=x&state=once&code=code",
      "dev.apell.shell://other/callback?iss=x&state=once&code=code",
      "dev.apell.shell://user@oauth/callback?iss=x&state=once&code=code",
      "dev.apell.shell://oauth/callback?iss=x&state=once&state=twice&code=code",
      "dev.apell.shell://oauth/callback?iss=x&iss=y&state=once&code=code",
      "dev.apell.shell://oauth/callback?state=once&code=code",
      "dev.apell.shell://oauth/callback?iss=x&state=once&code=code&error=access_denied",
      "dev.apell.shell://oauth/callback?iss=x&state=once&code=code#fragment",
    ] {
      assert!(callback(&url(wrong)).is_none());
    }
  }

  #[test]
  fn signed_es384_identity_binds_nonce_audience_and_access_token() {
    let config = Config {
      issuer: Url::parse("https://account.example.invalid/oidc").unwrap(),
      client_id: "fixture-native",
      sync_url: Url::parse("https://sync.example.invalid/").unwrap(),
      resource: "https://sync.example.invalid/config-sync",
    };
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
      "nonce": "bound-nonce", "iat": now, "exp": now + 60,
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
      verify_with_keys(&config, &tokens, "bound-nonce", &jwks)
        .unwrap()
        .sub,
      "signed-subject"
    );
    assert!(verify_with_keys(&config, &tokens, "other-nonce", &jwks).is_err());
    tokens.access_token = "other-access".into();
    assert!(verify_with_keys(&config, &tokens, "bound-nonce", &jwks).is_err());
    tokens.access_token = access_token.into();
    let mut wrong_audience = claims;
    wrong_audience["aud"] = json!("other-client");
    tokens.id_token = Some(encode(&header, &wrong_audience, &signer).unwrap());
    assert!(verify_with_keys(&config, &tokens, "bound-nonce", &jwks).is_err());
  }
}

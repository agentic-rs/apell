use super::{input, PreparedPublish, PublishArgs};
use input::AuthMethod;
use reqwest::{
  blocking::{Client, Response},
  header::{HeaderValue, AUTHORIZATION},
  redirect::Policy,
};
use serde::Deserialize;
use std::{
  io::Read,
  time::{Duration, SystemTime, UNIX_EPOCH},
};
use url::Url;

const MAX_IDENTITY_BYTES: usize = 16 * 1024;
const MAX_AUTH_RESPONSE_BYTES: usize = 48 * 1024;

pub struct Credential {
  pub value: String,
  redactions: Vec<String>,
}

impl Credential {
  pub fn redact(&self, message: String) -> String {
    self.redactions.iter().fold(
      message.replace(&self.value, "[redacted]"),
      |text, secret| text.replace(secret, "[redacted]"),
    )
  }
}

pub fn credential(args: &PublishArgs, prepared: &PreparedPublish) -> Result<Credential, String> {
  let github_actions = std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true");
  match method(args.auth, github_actions) {
    AuthMethod::Token => {
      let path = std::env::var_os("APELL_STORE_PUBLISH_TOKEN_FILE")
        .filter(|value| !value.is_empty())
        .ok_or("Set APELL_STORE_PUBLISH_TOKEN_FILE to a file containing the publishing token.")?;
      Ok(Credential {
        value: input::read_token(std::path::Path::new(&path))?,
        redactions: Vec::new(),
      })
    }
    AuthMethod::GithubOidc => {
      let audience = store_audience(&args.endpoint)?;
      let request_url = std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL")
        .map_err(|_| "GitHub Actions OIDC is unavailable. Grant this job id-token: write.")?;
      let request_token = std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
        .map_err(|_| "GitHub Actions OIDC is unavailable. Grant this job id-token: write.")?;
      let endpoint = identity_endpoint(&request_url, &audience)?;
      let client = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "Could not initialize GitHub Actions authentication.")?;
      let identity = request_identity(&client, endpoint, &request_token)?;
      Ok(Credential {
        value: exchange(&client, &args.endpoint, &identity, prepared)?,
        redactions: vec![identity, request_token],
      })
    }
  }
}

fn method(explicit: Option<AuthMethod>, github_actions: bool) -> AuthMethod {
  explicit.unwrap_or(if github_actions {
    AuthMethod::GithubOidc
  } else {
    AuthMethod::Token
  })
}

fn store_audience(endpoint: &Url) -> Result<String, String> {
  if endpoint.scheme() != "https" {
    return Err("GitHub Actions publishing requires an HTTPS store address.".into());
  }
  Ok(endpoint.origin().ascii_serialization())
}

fn identity_endpoint(value: &str, audience: &str) -> Result<Url, String> {
  const INVALID: &str = "GitHub supplied an invalid OIDC request address.";
  if value.len() > 8_192
    || value
      .chars()
      .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
  {
    return Err(INVALID.into());
  }
  let mut url = Url::parse(value).map_err(|_| INVALID)?;
  let authority = value.split_once("://").ok_or(INVALID)?.1;
  let authority = authority.split(['/', '?', '#']).next().ok_or(INVALID)?;
  if url.scheme() != "https"
    || !url.host_str().is_some_and(|host| {
      host
        .strip_suffix(".actions.githubusercontent.com")
        .is_some_and(|prefix| !prefix.is_empty())
    })
    || url.port_or_known_default() != Some(443)
    || authority.contains('@')
    || !url.username().is_empty()
    || url.password().is_some()
    || url.fragment().is_some()
    || url.path() == "/"
  {
    return Err(INVALID.into());
  }
  let query: Vec<(String, String)> = url
    .query_pairs()
    .filter(|(key, _)| key != "audience")
    .map(|(key, value)| (key.into_owned(), value.into_owned()))
    .collect();
  url.set_query(None);
  url
    .query_pairs_mut()
    .extend_pairs(query)
    .append_pair("audience", audience);
  Ok(url)
}

fn bearer(value: &str, maximum: usize) -> Result<HeaderValue, String> {
  if value.is_empty() || value.len() > maximum || !value.bytes().all(|c| c.is_ascii_graphic()) {
    return Err("Invalid authentication credential.".into());
  }
  let mut header = HeaderValue::from_str(&format!("Bearer {value}"))
    .map_err(|_| "Invalid authentication credential.")?;
  header.set_sensitive(true);
  Ok(header)
}

fn auth_body(response: Response, service: &str) -> Result<Vec<u8>, String> {
  if !response.status().is_success() {
    // Authentication bodies may contain echoed credentials; do not print them.
    return Err(format!(
      "{service} rejected GitHub Actions authentication (HTTP {}). Check the job permissions and trusted publisher configuration. No token fallback was attempted.",
      response.status().as_u16()
    ));
  }
  if response
    .content_length()
    .is_some_and(|size| size > MAX_AUTH_RESPONSE_BYTES as u64)
  {
    return Err(format!(
      "{service} returned an oversized authentication response."
    ));
  }
  let mut bytes = Vec::new();
  response
    .take(MAX_AUTH_RESPONSE_BYTES as u64 + 1)
    .read_to_end(&mut bytes)
    .map_err(|_| format!("Could not read the {service} authentication response."))?;
  if bytes.len() > MAX_AUTH_RESPONSE_BYTES {
    return Err(format!(
      "{service} returned an oversized authentication response."
    ));
  }
  Ok(bytes)
}

fn request_identity(client: &Client, endpoint: Url, request_token: &str) -> Result<String, String> {
  #[derive(Deserialize)]
  struct Identity {
    value: String,
  }
  let response = client
    .get(endpoint)
    .header(AUTHORIZATION, bearer(request_token, MAX_IDENTITY_BYTES)?)
    .send()
    .map_err(|_| {
      "Could not request a GitHub Actions identity token. No token fallback was attempted."
    })?;
  let body = auth_body(response, "GitHub")?;
  let identity: Identity =
    serde_json::from_slice(&body).map_err(|_| "GitHub returned an invalid identity response.")?;
  let parts: Vec<_> = identity.value.split('.').collect();
  if identity.value.len() > MAX_IDENTITY_BYTES
    || parts.len() != 3
    || parts.iter().any(|part| {
      part.is_empty()
        || !part
          .bytes()
          .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    })
  {
    return Err("GitHub returned an invalid identity token.".into());
  }
  Ok(identity.value)
}

#[derive(Deserialize)]
struct ExchangeResponse {
  token: String,
  expires_at: u64,
}

fn exchange(
  client: &Client,
  publish: &Url,
  identity: &str,
  prepared: &PreparedPublish,
) -> Result<String, String> {
  let mut endpoint = publish.clone();
  endpoint.set_path("/api/v1/auth/github-actions");
  let response = client.post(endpoint)
    .header(AUTHORIZATION, bearer(identity, MAX_IDENTITY_BYTES)?)
    .json(&serde_json::json!({
      "applet_id": prepared.manifest.applet_id,
      "sha256": prepared.sha256,
    }))
    .send()
    .map_err(|_| "Could not exchange the GitHub Actions identity with the store. No token fallback was attempted.")?;
  let body = auth_body(response, "Store")?;
  let result: ExchangeResponse = serde_json::from_slice(&body)
    .map_err(|_| "The store returned an invalid authentication response.")?;
  let now = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_err(|_| "The system clock is invalid.")?
    .as_secs();
  if !(32..=512).contains(&result.token.len())
    || !result.token.bytes().all(|c| c.is_ascii_graphic())
    || result.expires_at <= now
    || result.expires_at > now.saturating_add(600)
  {
    return Err("The store returned an invalid or expired publishing credential.".into());
  }
  Ok(result.token)
}

#[cfg(test)]
mod tests;

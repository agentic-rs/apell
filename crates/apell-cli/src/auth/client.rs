use super::args::is_loopback;
use reqwest::{
  blocking::{Client, Response},
  header::{HeaderValue, ACCEPT, AUTHORIZATION},
  redirect::Policy,
  Method,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{io::Read, time::Duration};
use url::Url;

const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

pub(super) struct Api {
  client: Client,
  endpoint: Url,
  token: HeaderValue,
}

impl Api {
  pub fn from_environment() -> Result<Self, String> {
    let endpoint = secure_url(&required("ACCOUNT_MANAGEMENT_ENDPOINT")?)?;
    if endpoint.path() != "/" {
      return Err(
        "Set ACCOUNT_MANAGEMENT_ENDPOINT to the Logto core origin without a path.".into(),
      );
    }
    let token = required("ACCOUNT_MANAGEMENT_TOKEN")?;
    if token.len() > 16 * 1024 || !token.bytes().all(|byte| byte.is_ascii_graphic()) {
      return Err("Invalid ACCOUNT_MANAGEMENT_TOKEN.".into());
    }
    let mut token = HeaderValue::from_str(&format!("Bearer {token}"))
      .map_err(|_| "Invalid ACCOUNT_MANAGEMENT_TOKEN.")?;
    token.set_sensitive(true);
    let mut client = Client::builder()
      .user_agent("Apell-CLI")
      .redirect(Policy::none())
      .connect_timeout(Duration::from_secs(10))
      .timeout(Duration::from_secs(30));
    if is_loopback(&endpoint) {
      client = client.no_proxy();
    }
    Ok(Self {
      endpoint,
      token,
      client: client
        .build()
        .map_err(|_| "Could not initialize the Account client.")?,
    })
  }

  pub fn discover<T: DeserializeOwned>(&self) -> Result<T, String> {
    let response = self.send(
      Method::GET,
      "/oidc/.well-known/openid-configuration",
      None,
      false,
    )?;
    decode(response)
  }

  pub fn json<T: DeserializeOwned>(
    &self,
    method: Method,
    path: &str,
    body: Option<&Value>,
  ) -> Result<T, String> {
    decode(self.send(method, path, body, true)?)
  }

  pub fn empty(&self, method: Method, path: &str, body: Option<&Value>) -> Result<(), String> {
    self.send(method, path, body, true)?;
    Ok(())
  }

  fn send(
    &self,
    method: Method,
    path: &str,
    body: Option<&Value>,
    authenticated: bool,
  ) -> Result<Response, String> {
    let mut url = self.endpoint.clone();
    url.set_path(path);
    let mut request = self
      .client
      .request(method, url)
      .header(ACCEPT, "application/json");
    if authenticated {
      request = request.header(AUTHORIZATION, self.token.clone());
    }
    if let Some(body) = body {
      request = request.json(body);
    }
    let response = request.send().map_err(|_| {
      "Could not contact Apell Account. If registration was submitted, inspect Account applications before retrying."
    })?;
    if !response.status().is_success() {
      // Management API error bodies can contain client credentials.
      return Err(format!(
        "Apell Account rejected the request (HTTP {}). Check the operator credential and registration options. Inspect Account applications before retrying registration.",
        response.status().as_u16()
      ));
    }
    Ok(response)
  }
}

fn decode<T: DeserializeOwned>(response: Response) -> Result<T, String> {
  if response
    .content_length()
    .is_some_and(|length| length > MAX_RESPONSE_BYTES)
  {
    return Err("The Account response is too large.".into());
  }
  let mut bytes = Vec::new();
  response.take(MAX_RESPONSE_BYTES + 1).read_to_end(&mut bytes)
    .map_err(|_| "Could not read the Account response. Inspect Account applications before retrying registration.")?;
  if bytes.len() as u64 > MAX_RESPONSE_BYTES {
    return Err("The Account response is too large.".into());
  }
  serde_json::from_slice(&bytes)
    .map_err(|_| "Apell Account returned an invalid response. Inspect Account applications before retrying registration.".into())
}

pub(super) fn secure_url(value: &str) -> Result<Url, String> {
  let url = Url::parse(value).map_err(|_| "Invalid Account endpoint or issuer URL.")?;
  if (url.scheme() != "https" && !(url.scheme() == "http" && is_loopback(&url)))
    || !url.username().is_empty()
    || url.password().is_some()
    || url.query().is_some()
    || url.fragment().is_some()
    || value
      .chars()
      .any(|character| character.is_whitespace() || character.is_control())
  {
    return Err(
      "Use an HTTPS or loopback HTTP Account URL without credentials, query, or fragment.".into(),
    );
  }
  Ok(url)
}

fn required(name: &str) -> Result<String, String> {
  std::env::var(name)
    .ok()
    .map(|value| value.trim().to_owned())
    .filter(|value| !value.is_empty())
    .ok_or_else(|| format!("Set {name} in the operator environment."))
}

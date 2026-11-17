use reqwest::{
  blocking::Client,
  header::{HeaderValue, AUTHORIZATION},
  redirect::Policy,
  Method, StatusCode,
};
use serde_json::Value;
use std::{io::Read, time::Duration};
use url::Url;

const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

pub(super) struct Api {
  client: Client,
  store: Url,
  token: Option<String>,
}

pub(super) struct Reply {
  pub status: StatusCode,
  pub value: Value,
}

impl Api {
  pub fn new(store: &Url, token: Option<&str>) -> Result<Self, String> {
    let mut client = Client::builder()
      .redirect(Policy::none())
      .connect_timeout(Duration::from_secs(10))
      .timeout(Duration::from_secs(30));
    if store.scheme() == "http" {
      client = client.no_proxy();
    }
    Ok(Self {
      client: client
        .build()
        .map_err(|_| "Could not initialize the Store client.")?,
      store: store.clone(),
      token: token.map(str::to_owned),
    })
  }

  pub fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Reply, String> {
    let mut url = self.store.clone();
    url.set_path(path);
    let mut request = self.client.request(method, url);
    if let Some(token) = &self.token {
      let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| "Invalid account credential.")?;
      header.set_sensitive(true);
      request = request.header(AUTHORIZATION, header);
    }
    if let Some(body) = body {
      request = request.json(body);
    }
    let response = request.send().map_err(|_| "Could not contact the Store. Check the connection; before retrying a change, inspect the package state.")?;
    let status = response.status();
    if status.is_redirection() {
      return Err("The Store returned a redirect. CLI redirects are disabled.".into());
    }
    if response
      .content_length()
      .is_some_and(|length| length > MAX_RESPONSE_BYTES)
    {
      return Err("The Store response is too large.".into());
    }
    let mut bytes = Vec::new();
    response
      .take(MAX_RESPONSE_BYTES + 1)
      .read_to_end(&mut bytes)
      .map_err(|_| "Could not read the Store response.")?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
      return Err("The Store response is too large.".into());
    }
    let mut value: Value =
      serde_json::from_slice(&bytes).map_err(|_| "The Store returned an invalid response.")?;
    if let Some(token) = &self.token {
      redact(&mut value, token);
    }
    Ok(Reply { status, value })
  }

  pub fn call(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value, String> {
    let reply = self.request(method, path, body)?;
    if reply.status.is_success() {
      Ok(reply.value)
    } else {
      Err(reply.error())
    }
  }
}

impl Reply {
  pub fn code(&self) -> Option<&str> {
    self.value.get("error")?.get("code")?.as_str()
  }
  pub fn error(&self) -> String {
    let explanation = match self.code() {
      Some("authorization_denied") => "The browser authorization was declined.",
      Some("authorization_expired") => "The browser authorization expired. Run apell login again.",
      Some("slow_down") => "The Store asked the CLI to retry more slowly.",
      Some("rate_limited") => "Too many sign-in attempts. Try again in a minute.",
      Some("store_busy") => "The Store is busy or its authorization limit was reached. Revoke unused CLI access in My packages, or try again later.",
      Some("unauthorized" | "sign_in_required") => "The account session is invalid or expired. Run apell login again.",
      Some("forbidden") => "Your account does not have permission for this package operation.",
      Some("not_found") => "The package, invitation, or member was not found.",
      Some("conflict") => "The requested change conflicts with the current package state.",
      Some("invalid_request") => "The Store rejected the request. Check the command arguments.",
      _ => "The Store rejected the request.",
    };
    // Do not echo server error bodies: an authentication error can contain credentials.
    format!("{explanation} (HTTP {})", self.status.as_u16())
  }
}

pub(super) fn redact(value: &mut Value, secret: &str) {
  match value {
    Value::String(text) => *text = text.replace(secret, "[redacted]"),
    Value::Array(values) => values.iter_mut().for_each(|value| redact(value, secret)),
    Value::Object(values) => {
      let old = std::mem::take(values);
      for (key, mut value) in old {
        redact(&mut value, secret);
        values.insert(key.replace(secret, "[redacted]"), value);
      }
    }
    _ => (),
  }
}

use super::{
  auth::validate_credential, default_base_url, driver_id, is_device_provider, AccountConfig, Client,
};
use crate::llm::LlmConfig;
use serde::Serialize;
use serde_json::Value;
use std::{collections::BTreeMap, time::Duration};
use tokn_core::{
  headers::{HeaderMap, TemplateVars},
  provider::{HeaderPatchCtx, ProviderRequestKind},
  upstream_url::{CanonicalUpstreamUrl, CleartextHttpPolicy},
  AgentId,
};

const MAX_MODEL_BYTES: usize = 4 * 1024 * 1024;
const MAX_MODELS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProviderModel {
  pub model_id: String,
  pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
  Provider,
  Catalogue,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelList {
  pub models: Vec<ProviderModel>,
  pub source: ModelSource,
}

/// Discover only at this connection's destination. The host supplies a refreshed
/// credential snapshot; discovery never refreshes or persists credentials.
pub async fn discover_models(
  http: &Client,
  config: &LlmConfig,
  account: Option<AccountConfig>,
) -> Result<ModelList, String> {
  if config.provider_id == "demo" {
    return Ok(ModelList {
      models: vec![ProviderModel {
        model_id: "local-demo".into(),
        name: "Local demo".into(),
      }],
      source: ModelSource::Catalogue,
    });
  }
  let (url, headers) = model_request(config, account.as_ref())?;
  let mut request = http.get(url).timeout(Duration::from_secs(30));
  for (name, value) in headers.iter() {
    request = request.header(name.as_str(), value.as_str());
  }
  let mut response = request
    .send()
    .await
    .map_err(|_| "Could not reach the model list endpoint. Try again.".to_owned())?;
  if !response.status().is_success() {
    return Err(
      match response.status().as_u16() {
        401 | 403 => "The provider rejected the credentials. Update the API key or sign in again.",
        404 | 405 | 501 => {
          "This connection does not provide a model list. Enter the model ID manually."
        }
        429 => "The provider is rate limiting model lookup. Try again later.",
        300..=399 => "The model list endpoint redirected. Check the connection URL.",
        _ => {
          "The provider could not return its model list. Try again or enter a model ID manually."
        }
      }
      .into(),
    );
  }
  if response
    .content_length()
    .is_some_and(|length| length > MAX_MODEL_BYTES as u64)
  {
    return Err("The provider model list is too large.".into());
  }
  let mut body = Vec::new();
  while let Some(chunk) = response
    .chunk()
    .await
    .map_err(|_| "Could not read the provider model list.".to_owned())?
  {
    if body.len().saturating_add(chunk.len()) > MAX_MODEL_BYTES {
      return Err("The provider model list is too large.".into());
    }
    body.extend_from_slice(&chunk);
  }
  let value = serde_json::from_slice(&body)
    .map_err(|_| "The provider returned an invalid model list.".to_owned())?;
  Ok(ModelList {
    models: parse_models(&config.provider_id, &value)?,
    source: ModelSource::Provider,
  })
}

fn model_request(
  config: &LlmConfig,
  account: Option<&AccountConfig>,
) -> Result<(reqwest::Url, HeaderMap), String> {
  let driver = driver_id(&config.provider_id)?;
  // Apell permits HTTP only for loopback development. Use Tokn's strict prefix
  // parser after that check, retaining support for the localhost spelling.
  if config.base_url.len() > 2048 {
    return Err("The provider URL is too long.".into());
  }
  let parsed =
    reqwest::Url::parse(&config.base_url).map_err(|_| "Enter a valid provider URL.".to_owned())?;
  let host = parsed.host_str().unwrap_or("");
  let loopback = host == "localhost"
    || host
      .trim_matches(['[', ']'])
      .parse::<std::net::IpAddr>()
      .is_ok_and(|address| address.is_loopback());
  if parsed.scheme() != "https" && !(parsed.scheme() == "http" && loopback) {
    return Err(
      "Provider URLs must use HTTPS, except HTTP on loopback for local development.".into(),
    );
  }
  let base =
    CanonicalUpstreamUrl::parse(parsed.as_str(), CleartextHttpPolicy::Allow).map_err(|_| {
      "Enter a valid provider base URL without query parameters or fragments.".to_owned()
    })?;
  if is_device_provider(&config.provider_id) {
    let expected = CanonicalUpstreamUrl::parse(
      default_base_url(&config.provider_id).unwrap(),
      CleartextHttpPolicy::LoopbackOnly,
    )
    .map_err(|_| "The provider default URL is invalid.".to_owned())?;
    if base != expected {
      return Err("Device sign-in requires this provider's default URL.".into());
    }
  }
  let mut headers = HeaderMap::new();
  if let Some(account) = account {
    if account.id != config.connection_id || account.provider != driver {
      return Err(
        "The saved credentials do not match this connection. Sign in or save the key again.".into(),
      );
    }
    if let Some(credential) = tokn_accounts::native::credential(driver, account) {
      validate_credential(credential)?;
      if driver == "github-copilot" {
        // Copilot's native normalizer preserves a supplied identity; it does
        // not create the model endpoint's required editor/integration headers.
        let identity = tokn_provider_copilot::config::CopilotHeaders::from_account(account)
          .map_err(|_| "The saved Copilot settings are invalid. Sign in again.".to_owned())?;
        identity
          .validate()
          .map_err(|_| "The saved Copilot settings are invalid. Sign in again.".to_owned())?;
        headers = tokn_provider_copilot::headers::copilot_request_headers(
          credential, &identity, false, "user",
        )
        .map_err(|_| "Could not prepare Copilot model discovery.".to_owned())?;
      }
      let context = HeaderPatchCtx {
        request_kind: ProviderRequestKind::Models,
        body: &Value::Null,
        bearer_token: Some(credential),
        content_encoding: None,
        stream: false,
        initiator: "user",
        inbound_headers: &HeaderMap::new(),
        vars: &TemplateVars::default(),
        agent_id: &AgentId::provider_default(driver).unwrap_or_default(),
      };
      // Native helpers do not construct a Provider, whose default metadata
      // would implicitly open Tokn's catalogue cache in the user's home.
      tokn_accounts::native::patch_headers(driver, &mut headers, &context, account).map_err(
        |_| "Could not prepare the provider credentials. Sign in or save the key again.".to_owned(),
      )?;
    } else if !matches!(config.provider_id.as_str(), "custom" | "llama-cpp") {
      return Err("Add credentials to this connection first.".into());
    }
  } else if !matches!(config.provider_id.as_str(), "custom" | "llama-cpp") {
    return Err("Add credentials to this connection first.".into());
  }
  headers.insert("accept", "application/json");
  let mut url = base
    .operation_url(["models"])
    .map_err(|_| "Could not construct the provider model list URL.".to_owned())?;
  if driver == "codex" {
    // The same Tokn identity version used by its Codex model discovery. Avoid a
    // separately hard-coded version that can drift when the SDK is updated.
    let version = headers
      .get("version")
      .ok_or_else(|| "Could not prepare Codex model discovery.".to_owned())?;
    url
      .query_pairs_mut()
      .append_pair("client_version", version.as_str());
  }
  Ok((url, headers))
}

fn parse_models(provider_id: &str, value: &Value) -> Result<Vec<ProviderModel>, String> {
  let entries = value
    .get("data")
    .and_then(Value::as_array)
    .or_else(|| value.get("models").and_then(Value::as_array))
    .or_else(|| value.as_array())
    .or_else(|| {
      (provider_id == "codex")
        .then(|| {
          value
            .as_object()?
            .values()
            .find_map(|entry| entry.get("models")?.as_array())
        })
        .flatten()
    })
    .ok_or_else(|| {
      "The provider returned an invalid model list. Enter a model ID manually.".to_owned()
    })?;
  if entries.len() > MAX_MODELS {
    return Err("The provider model list contains too many models.".into());
  }
  let mut models = BTreeMap::new();
  for entry in entries {
    let Some(id) = entry.as_str().or_else(|| {
      ["id", "model", "slug", "name"]
        .iter()
        .find_map(|key| entry.get(key)?.as_str())
    }) else {
      continue;
    };
    if id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
      continue;
    }
    let name = ["display_name", "name"]
      .iter()
      .find_map(|key| entry.get(key)?.as_str())
      .filter(|name| {
        !name.trim().is_empty() && name.len() <= 512 && !name.chars().any(char::is_control)
      })
      .unwrap_or(id);
    models
      .entry(id.to_owned())
      .or_insert_with(|| ProviderModel {
        model_id: id.into(),
        name: name.into(),
      });
  }
  if !entries.is_empty() && models.is_empty() {
    return Err("The provider returned no valid model IDs. Enter a model ID manually.".into());
  }
  Ok(models.into_values().collect())
}

#[cfg(test)]
mod tests;

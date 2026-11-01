//! Embedded provider setup. Credentials stay native; no Tokn stores are opened.

mod auth;
mod models;

pub use auth::{begin_login, device_account, poll_login, refresh_account, static_account};
pub use models::{discover_models, ModelList, ModelSource, ProviderModel};
pub use reqwest::Client;
pub use tokn_auth::{DeviceCodeHandle, DeviceFlowOutcome};
pub use tokn_sdk::account::AccountConfig;

use serde::Serialize;
use std::time::Duration;
use tokn_accounts::registry::Registry;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
  ApiKey,
  DeviceCode,
  None,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProviderPreset {
  pub provider_id: String,
  pub name: String,
  pub base_url: String,
  pub auth_methods: Vec<AuthMethod>,
}

/// Only implemented drivers are advertised, not every models.dev provider.
pub fn presets() -> Vec<ProviderPreset> {
  let mut providers = vec![
    ProviderPreset {
      provider_id: "demo".into(),
      name: "Local demo".into(),
      base_url: "demo".into(),
      auth_methods: vec![AuthMethod::None],
    },
    ProviderPreset {
      provider_id: "custom".into(),
      name: "OpenAI compatible".into(),
      base_url: "https://api.openai.com/v1".into(),
      auth_methods: vec![AuthMethod::ApiKey, AuthMethod::None],
    },
  ];
  providers.extend(Registry::builtin().iter().map(|descriptor| ProviderPreset {
    provider_id: descriptor.id.into(),
    name: descriptor.display_name.into(),
    base_url: descriptor.base_url.into(),
    auth_methods: if is_device_provider(descriptor.id) {
      vec![AuthMethod::DeviceCode]
    } else if descriptor.id == "llama-cpp" {
      vec![AuthMethod::ApiKey, AuthMethod::None]
    } else {
      vec![AuthMethod::ApiKey]
    },
  }));
  providers
}

pub fn default_base_url(provider_id: &str) -> Option<&'static str> {
  match provider_id {
    "demo" => Some("demo"),
    "custom" => Some("https://api.openai.com/v1"),
    _ => tokn_accounts::native::default_base_url(provider_id),
  }
}

pub fn is_device_provider(provider_id: &str) -> bool {
  matches!(provider_id, "codex" | "github-copilot")
}

fn driver_id(provider_id: &str) -> Result<&str, String> {
  if provider_id == "custom" {
    Ok("openai")
  } else if Registry::builtin().resolve(provider_id).is_some() {
    Ok(provider_id)
  } else {
    Err("Choose a supported provider.".into())
  }
}

/// Shared TLS policy, no redirect credential forwarding, and bounded HTTP calls.
/// The host separately bounds the entire device-login session across poll calls.
pub fn http_client() -> Result<Client, String> {
  crate::http::client_builder()?
    .redirect(reqwest::redirect::Policy::none())
    .connect_timeout(Duration::from_secs(10))
    .read_timeout(Duration::from_secs(30))
    .timeout(Duration::from_secs(30))
    .build()
    .map_err(|_| "Could not initialize provider networking.".into())
}

#[cfg(test)]
mod tests;

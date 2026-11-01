use super::*;
use std::collections::HashSet;

#[test]
fn presets_advertise_only_supported_embedded_auth_and_exact_defaults() {
  let providers = presets();
  let ids = providers
    .iter()
    .map(|provider| provider.provider_id.as_str())
    .collect::<HashSet<_>>();
  assert_eq!(ids.len(), providers.len());
  assert_eq!(providers.len(), Registry::builtin().ids().len() + 2);
  for provider in &providers {
    assert_eq!(
      Some(provider.base_url.as_str()),
      default_base_url(&provider.provider_id)
    );
    assert!(!provider.name.is_empty());
    assert!(!provider.auth_methods.is_empty());
    if is_device_provider(&provider.provider_id) {
      assert_eq!(provider.auth_methods, [AuthMethod::DeviceCode]);
    }
  }
  let serialized = serde_json::to_value(providers).unwrap();
  let copilot = serialized
    .as_array()
    .unwrap()
    .iter()
    .find(|row| row["provider_id"] == "github-copilot")
    .unwrap();
  assert_eq!(copilot["auth_methods"], serde_json::json!(["device_code"]));
  assert_eq!(copilot["base_url"], "https://api.githubcopilot.com");
  assert!(default_base_url("unknown").is_none());
}

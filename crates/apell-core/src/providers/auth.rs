use super::{
  driver_id, is_device_provider, AccountConfig, Client, DeviceCodeHandle, DeviceFlowOutcome,
};
use std::{
  future::Future,
  time::{SystemTime, UNIX_EPOCH},
};
use tokn_accounts::auth_registry::provider_auth_for;
use tokn_auth::{AuthError, ProviderAuth, RefreshOutcome};
use tokn_sdk::account::Secret;
use tracing::{instrument::WithSubscriber, subscriber::NoSubscriber};

const MAX_CREDENTIAL_BYTES: usize = 32 * 1024;

fn auth_provider(provider_id: &str) -> Result<&'static dyn ProviderAuth, String> {
  if !is_device_provider(provider_id) {
    return Err("This provider does not support device sign-in.".into());
  }
  provider_auth_for(provider_id)
    .filter(|auth| auth.supports_device_flow())
    .ok_or_else(|| "This provider does not support device sign-in.".into())
}

pub async fn begin_login(provider_id: &str, http: &Client) -> Result<DeviceCodeHandle, String> {
  let handle = without_auth_tracing(auth_provider(provider_id)?.request_device_code(http))
    .await
    .map_err(auth_error)?;
  validate_device_handle(provider_id, &handle)?;
  Ok(handle)
}

/// The host owns cancellation and an expiry measured from begin_login, not poll.
pub async fn poll_login(
  provider_id: &str,
  http: &Client,
  handle: DeviceCodeHandle,
) -> Result<DeviceFlowOutcome, String> {
  let auth = auth_provider(provider_id)?;
  validate_device_handle(provider_id, &handle)?;
  let outcome = without_auth_tracing(auth.poll_device_code(http, handle))
    .await
    .map_err(auth_error)?;
  validate_credential(&outcome.access_token)?;
  validate_credential(&outcome.refresh_token)?;
  if outcome.access_token_expires_at <= unix_time() {
    return Err("The provider returned an expired credential. Sign in again.".into());
  }
  Ok(outcome)
}

fn validate_device_handle(provider_id: &str, handle: &DeviceCodeHandle) -> Result<(), String> {
  let url = reqwest::Url::parse(&handle.verification_uri)
    .map_err(|_| "The provider returned an invalid sign-in link.".to_owned())?;
  // Only open the selected provider's HTTPS authorization site, never an
  // arbitrary URI from a response or a link containing embedded credentials.
  let expected_host = match provider_id {
    "codex" => "auth.openai.com",
    "github-copilot" => "github.com",
    _ => return Err("This provider does not support device sign-in.".into()),
  };
  if url.scheme() != "https"
    || url.host_str() != Some(expected_host)
    || url.port_or_known_default() != Some(443)
    || !url.username().is_empty()
    || url.password().is_some()
    || url.fragment().is_some()
  {
    return Err("The provider returned an invalid sign-in link.".into());
  }
  if handle.user_code.is_empty()
    || handle.user_code.len() > 128
    || handle.user_code.chars().any(char::is_control)
    || handle.device_code.is_empty()
    || handle.device_code.len() > MAX_CREDENTIAL_BYTES
    || handle.expires_in == 0
    || handle.expires_in > 3600
    || handle.interval > 60
  {
    return Err("The provider returned an invalid sign-in code.".into());
  }
  Ok(())
}

pub fn device_account(
  provider_id: &str,
  connection_id: &str,
  outcome: DeviceFlowOutcome,
) -> Result<AccountConfig, String> {
  auth_provider(provider_id)?;
  validate_credential(&outcome.access_token)?;
  validate_credential(&outcome.refresh_token)?;
  if outcome.access_token_expires_at <= unix_time() {
    return Err("The provider returned an expired credential. Sign in again.".into());
  }
  let mut account = empty_account(provider_id, connection_id)?;
  account.access_token = Some(Secret::new(outcome.access_token));
  account.access_token_expires_at = Some(outcome.access_token_expires_at);
  account.refresh_token = Some(Secret::new(outcome.refresh_token));
  account.username = outcome.username;
  account.provider_account_id = outcome.provider_account_id;
  account.last_refresh = Some(unix_time());
  Ok(account)
}

pub fn static_account(
  provider_id: &str,
  connection_id: &str,
  api_key: Option<&str>,
) -> Result<AccountConfig, String> {
  if is_device_provider(provider_id) {
    return Err("Use device sign-in for this provider.".into());
  }
  let mut account = empty_account(provider_id, connection_id)?;
  match api_key.filter(|key| !key.is_empty()) {
    Some(key) => {
      validate_credential(key)?;
      account.api_key = Some(Secret::new(key.to_owned()));
    }
    None if matches!(provider_id, "custom" | "llama-cpp") => {}
    None => return Err("Enter an API key for this provider.".into()),
  }
  Ok(account)
}

fn empty_account(provider_id: &str, connection_id: &str) -> Result<AccountConfig, String> {
  if connection_id.is_empty()
    || connection_id.len() > 128
    || !connection_id
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
  {
    return Err("The connection identity is invalid.".into());
  }
  // Serde supplies Tokn's defaults without copying its evolving account schema.
  serde_json::from_value(serde_json::json!({
    "id": connection_id,
    "provider": driver_id(provider_id)?,
  }))
  .map_err(|_| "Could not initialize provider credentials.".into())
}

/// Serialize calls per connection and persist a changed account to the secure
/// vault before using it. A failed exchange leaves the supplied account intact.
pub async fn refresh_account(http: &Client, account: &mut AccountConfig) -> Result<bool, String> {
  driver_id(&account.provider)?;
  if !is_device_provider(&account.provider) {
    return Ok(false);
  }
  let auth = auth_provider(&account.provider)?;
  if account
    .refresh_url
    .as_deref()
    .is_some_and(|url| Some(url) != auth.default_refresh_url())
  {
    return Err("This provider requires its default sign-in endpoint.".into());
  }
  let refresh_token = account
    .refresh_token
    .as_ref()
    .ok_or_else(|| "Sign in to this provider again.".to_owned())?;
  validate_credential(refresh_token.expose())?;
  // Copilot's ProviderAuth default always exchanges. Respect the same five-
  // minute margin as its runtime to avoid unnecessary token exchanges.
  if account.provider == "github-copilot"
    && account
      .access_token
      .as_ref()
      .is_some_and(|token| validate_credential(token.expose()).is_ok())
    && account
      .access_token_expires_at
      .is_some_and(|expiry| expiry.saturating_sub(300) > unix_time())
  {
    return Ok(false);
  }
  let outcome = without_auth_tracing(auth.refresh_credential_if_needed(http, account))
    .await
    .map_err(auth_error)?;
  apply_refresh(account, outcome)
}

async fn without_auth_tracing<T>(operation: impl Future<Output = T>) -> T {
  // Tokn's device flow traces user codes and token fingerprints. Suppress the
  // provider future on every poll, restoring the app's subscriber afterwards.
  operation.with_subscriber(NoSubscriber::default()).await
}

fn apply_refresh(account: &mut AccountConfig, outcome: RefreshOutcome) -> Result<bool, String> {
  if let RefreshOutcome::Refreshed {
    access_token,
    expires_at,
    refresh_token,
    ..
  } = &outcome
  {
    validate_credential(access_token)?;
    if let Some(token) = refresh_token
      .as_ref()
      .filter(|token| !token.trim().is_empty())
    {
      validate_credential(token)?;
    }
    if *expires_at <= unix_time() {
      return Err("The provider returned an expired credential. Sign in again.".into());
    }
  }
  Ok(outcome.apply_to(account))
}

pub(super) fn validate_credential(value: &str) -> Result<(), String> {
  if value.len() > MAX_CREDENTIAL_BYTES {
    return Err("The provider credential is too long.".into());
  }
  tokn_core::provider::validate_credential(value)
    .map_err(|_| "The provider credential is invalid. Enter it again or sign in again.".into())
}

// AuthError strings may contain credentials, user identities, or upstream body
// excerpts. Classify variants without interpolating or logging those strings.
fn auth_error(error: AuthError) -> String {
  match error {
    AuthError::Unsupported(_) => "This provider does not support this sign-in operation.",
    AuthError::MissingCredential { .. } => "Sign in to this provider again.",
    AuthError::Network(_) => "Could not reach the sign-in service. Try again.",
    AuthError::Upstream(_) => "The sign-in service rejected the request. Try signing in again.",
    AuthError::Decode(_) => "The sign-in service returned an invalid response. Try again.",
    AuthError::Other(_) => "Sign-in did not complete. The code may have expired or been declined.",
  }
  .into()
}

fn unix_time() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |duration| duration.as_secs() as i64)
}

#[cfg(test)]
mod tests;

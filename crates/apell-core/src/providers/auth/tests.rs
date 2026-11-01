use super::*;

fn handle() -> DeviceCodeHandle {
  DeviceCodeHandle {
    device_code: "private-device-handle".into(),
    user_code: "TEST-CODE".into(),
    verification_uri: "https://github.com/login/device".into(),
    expires_in: 900,
    interval: 5,
  }
}

fn outcome() -> DeviceFlowOutcome {
  DeviceFlowOutcome {
    refresh_token: "refresh-secret".into(),
    access_token: "access-secret".into(),
    access_token_expires_at: unix_time() + 3600,
    username: Some("account-label".into()),
    provider_account_id: Some("provider-account-id".into()),
  }
}

#[test]
fn sign_in_links_and_codes_are_validated_before_ui_or_polling() {
  assert!(validate_device_handle("github-copilot", &handle()).is_ok());
  for link in [
    "javascript:alert(1)",
    "http://github.com/login/device",
    "https://github.com.example.invalid/login/device",
    "https://user:password@github.com/login/device",
    "https://github.com:444/login/device",
  ] {
    let mut invalid = handle();
    invalid.verification_uri = link.into();
    assert!(validate_device_handle("github-copilot", &invalid).is_err());
  }
  assert!(validate_device_handle("codex", &handle()).is_err());
  let mut invalid = handle();
  invalid.expires_in = u64::MAX;
  assert!(validate_device_handle("github-copilot", &invalid).is_err());
}

#[test]
fn accounts_use_host_identity_and_expected_credential_flavor() {
  let oauth = device_account("codex", "connection-id", outcome()).unwrap();
  assert_eq!(oauth.id, "connection-id");
  assert_eq!(oauth.provider, "codex");
  assert_eq!(
    oauth.provider_account_id.as_deref(),
    Some("provider-account-id")
  );
  assert_eq!(oauth.access_token.unwrap().expose(), "access-secret");
  assert!(oauth.api_key.is_none());
  assert!(oauth.refresh_url.is_none());
  assert!(oauth.base_url.is_none());

  let custom = static_account("custom", "connection-id", Some("test-key")).unwrap();
  assert_eq!(custom.provider, "openai");
  assert_eq!(custom.api_key.unwrap().expose(), "test-key");
  assert!(custom.refresh_token.is_none());
  assert!(static_account("custom", "connection-id", None)
    .unwrap()
    .api_key
    .is_none());
  assert!(static_account("llama-cpp", "connection-id", None).is_ok());
  assert!(static_account("openai", "connection-id", None).is_err());
  assert!(static_account("codex", "connection-id", Some("test-key")).is_err());
  assert!(static_account("unknown", "connection-id", Some("test-key")).is_err());
  assert!(static_account("custom", "bad connection", Some("test-key")).is_err());
  assert!(static_account("custom", "connection-id", Some("key\r\nX-Injected: value")).is_err());
}

#[test]
fn refresh_persists_rotation_and_keeps_previous_account_on_invalid_result() {
  let mut account = device_account("codex", "connection-id", outcome()).unwrap();
  let refresh = RefreshOutcome::Refreshed {
    access_token: "new-access".into(),
    expires_at: unix_time() + 3600,
    refresh_token: Some("rotated-refresh".into()),
    id_token: Some("new-identity".into()),
    username: None,
    provider_account_id: None,
  };
  assert!(apply_refresh(&mut account, refresh.clone()).unwrap());
  assert_eq!(
    account.refresh_token.as_ref().unwrap().expose(),
    "rotated-refresh"
  );
  assert_eq!(account.id_token.as_ref().unwrap().expose(), "new-identity");
  assert_eq!(
    account.provider_account_id.as_deref(),
    Some("provider-account-id")
  );
  let before = serde_json::to_value(&account).unwrap();
  let invalid = RefreshOutcome::Refreshed {
    access_token: "invalid\nheader".into(),
    expires_at: unix_time() + 3600,
    refresh_token: Some("lost-refresh".into()),
    id_token: None,
    username: None,
    provider_account_id: None,
  };
  assert!(apply_refresh(&mut account, invalid).is_err());
  assert_eq!(serde_json::to_value(&account).unwrap(), before);
  assert!(!apply_refresh(&mut account, RefreshOutcome::Unchanged).unwrap());
  assert_eq!(serde_json::to_value(&account).unwrap(), before);
}

#[test]
fn authentication_errors_never_echo_remote_payloads_or_identity() {
  for error in [
    AuthError::Unsupported("private-payload".into()),
    AuthError::MissingCredential {
      account: "private-payload".into(),
      field: "refresh_token",
    },
    AuthError::Network("private-payload".into()),
    AuthError::Upstream("private-payload".into()),
    AuthError::Decode("private-payload".into()),
    AuthError::Other("private-payload".into()),
  ] {
    assert!(!auth_error(error).contains("private-payload"));
  }
}

#[tokio::test]
async fn fresh_copilot_credentials_and_static_keys_do_not_make_refresh_requests() {
  // A regression must fail locally, never send fixture tokens to a live service.
  let http = crate::http::client_builder()
    .unwrap()
    .no_proxy()
    .resolve("api.github.com", "127.0.0.1:1".parse().unwrap())
    .build()
    .unwrap();
  let mut oauth = device_account("github-copilot", "connection-id", outcome()).unwrap();
  let before = serde_json::to_value(&oauth).unwrap();
  assert!(!refresh_account(&http, &mut oauth).await.unwrap());
  assert_eq!(serde_json::to_value(&oauth).unwrap(), before);
  let mut static_key = static_account("openai", "connection-id", Some("test-key")).unwrap();
  assert!(!refresh_account(&http, &mut static_key).await.unwrap());
  oauth.refresh_url = Some("https://example.invalid/credential-sink".into());
  assert!(refresh_account(&http, &mut oauth).await.is_err());
  assert!(begin_login("custom", &http).await.is_err());
}

#[tokio::test]
async fn authentication_traces_stay_private_across_await_without_disabling_app_logs() {
  use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
  };
  use tracing::{span, Event, Metadata, Subscriber};

  struct Capture(Arc<AtomicUsize>);
  impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
      true
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
      span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
      if event.metadata().target() == "auth_scope_test" {
        self.0.fetch_add(1, Ordering::Relaxed);
      }
    }
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
  }

  let events = Arc::new(AtomicUsize::new(0));
  async {
    tracing::info!(target: "auth_scope_test", "app event before authentication");
    let result = without_auth_tracing(async {
      tracing::info!(target: "auth_scope_test", user_code = "fixture-private-code");
      tokio::task::yield_now().await;
      tracing::info!(target: "auth_scope_test", token = "fixture-private-token");
      42
    })
    .await;
    assert_eq!(result, 42);
    tracing::info!(target: "auth_scope_test", "app event after authentication");
  }
  .with_subscriber(Capture(Arc::clone(&events)))
  .await;
  assert_eq!(events.load(Ordering::Relaxed), 2);
}

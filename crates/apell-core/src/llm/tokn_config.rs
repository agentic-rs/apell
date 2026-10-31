use super::{validation::completion_url, LlmConfig};
use serde_json::json;
use tokn_sdk::{config::v2::compile_execution, policy::ExecutionRules};

/// One opaque route preserves Apell's compatible-provider contract. Credentials
/// arrive through host-owned RequestOptions, never an account file or catalogue.
pub(super) fn execution_rules(config: &LlmConfig) -> Result<ExecutionRules, String> {
  if config.base_url == "demo" {
    return Err("Demo mode does not use provider routing.".into());
  }

  // Start with Apell's effective URL so URL normalization and removal of trailing
  // slashes remain identical to the existing compatible-provider transport.
  let mut base_url = completion_url(config)?;
  let prefix = base_url
    .path()
    .strip_suffix("/chat/completions")
    .ok_or_else(|| "Could not prepare the model connection.".to_owned())?
    .to_owned();
  base_url.set_path(&prefix);
  // Tokn's default HTTP admission recognizes numeric loopback only. Apell has
  // already admitted this exact URL using its localhost-or-loopback policy.
  let allow_insecure_http = base_url.scheme() == "http";
  let raw = serde_json::from_value(json!({
    "schema_version": 2,
    "profiles": {
      "default": { "route": "apell", "wire_identity": "none" }
    },
    "routes": {
      "apell": {
        "kind": "relay",
        "providers": ["apell"],
        "destination": { "kind": "fixed_provider", "provider": "apell" },
        "credentials": { "kind": "client" },
        "retry": { "kind": "never" }
      }
    },
    "providers": {
      "apell": {
        "driver": "openai",
        "base_url": base_url.as_str(),
        "allow_insecure_http": allow_insecure_http
      }
    }
  }))
  .map_err(|_| "Could not prepare the model connection.".to_owned())?;
  compile_execution(&raw).map_err(|_| "Could not configure the model connection.".to_owned())
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::Value;
  use std::{cell::RefCell, time::Duration};
  use tokn_sdk::{
    policy::{ProviderId, RelayCredentials, RelayDestination, RelayRetry, RoutePlan, WireIdentity},
    transport::{Request, ResponseHead, Transport},
    Adapter, Client, Endpoint, RequestOptions, Runtime,
  };

  fn config(base_url: &str) -> LlmConfig {
    LlmConfig {
      base_url: base_url.into(),
      model_id: "namespace/custom-model:variant".into(),
      connection_id: "test_connection".into(),
    }
  }

  #[test]
  fn fixes_destination_without_account_pool_identity_or_retries() {
    let rules = execution_rules(&config("https://example.invalid/tenant/v1")).unwrap();
    assert_eq!(rules.profiles().len(), 1);
    assert_eq!(rules.routes().len(), 1);
    assert!(rules.account_pools().is_empty());
    let profile = rules.profiles().values().next().unwrap();
    assert_eq!(profile.wire_identity(), &WireIdentity::None);
    assert!(profile.account_pool().is_none());
    let route = rules.route(profile.route()).unwrap();
    let RoutePlan::Relay(relay) = route else {
      panic!("compatible requests must remain opaque");
    };
    let provider_id = ProviderId::new("apell").unwrap();
    assert_eq!(
      relay.destination(),
      &RelayDestination::FixedProvider(provider_id.clone())
    );
    assert_eq!(relay.credentials(), &RelayCredentials::Client);
    assert_eq!(relay.retry(), &RelayRetry::Never);
    assert!(route.allows_provider(&provider_id));
    assert!(!route.allows_provider(&ProviderId::new("openai").unwrap()));
    assert_eq!(
      rules.providers()[&provider_id].base_url(),
      Some("https://example.invalid/tenant/v1/")
    );
    assert!(!rules.providers()[&provider_id].allow_insecure_http());
  }

  #[test]
  fn keeps_apell_url_admission_before_enabling_tokn_http() {
    for url in [
      "http://example.invalid/v1",
      "http://192.0.2.1/v1",
      "http://localhost.example.invalid/v1",
      "https://user:secret@example.invalid/v1",
      "https://example.invalid/v1?key=secret",
      "https://example.invalid/v1#fragment",
      "demo",
    ] {
      assert!(execution_rules(&config(url)).is_err(), "accepted {url}");
    }
  }

  struct CaptureTransport<'a> {
    requests: &'a RefCell<Vec<Request>>,
    remaining: &'static [u8],
    fail: bool,
  }

  impl Transport for CaptureTransport<'_> {
    type Error = &'static str;

    async fn send(&mut self, request: Request) -> Result<ResponseHead, Self::Error> {
      self.requests.borrow_mut().push(request);
      if self.fail {
        return Err("fixture transport failure");
      }
      let mut head = ResponseHead {
        status: 200,
        headers: Default::default(),
      };
      head.headers.insert("content-type", "application/json");
      Ok(head)
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
      let count = buffer.len().min(self.remaining.len());
      buffer[..count].copy_from_slice(&self.remaining[..count]);
      self.remaining = &self.remaining[count..];
      Ok(count)
    }

    fn recoverable(&self, _error: &Self::Error) -> bool {
      true
    }
  }

  struct TestRuntime;

  impl Runtime for TestRuntime {
    fn now(&self) -> Duration {
      Duration::ZERO
    }

    fn unix_seconds(&self) -> u64 {
      0
    }

    async fn sleep(&mut self, _duration: Duration) {
      panic!("a non-retrying route must not wait to retry");
    }
  }

  #[tokio::test]
  async fn preserves_effective_url_model_and_optional_auth_without_accounts() {
    for url in [
      "https://api.openai.com/v1",
      "https://example.invalid/proxy%2Ftenant/v1///",
      "https://EXAMPLE.invalid/old/../v1",
      "http://localhost:4317/local/v1",
      "http://127.0.0.1:4317/local/v1",
      "http://127.1:4317/local/v1",
      "http://[::1]:4317/local/v1",
    ] {
      for key in [None, Some("test-key")] {
        let config = config(url);
        let requests = RefCell::new(Vec::new());
        let client = Client::new(
          execution_rules(&config).unwrap(),
          Vec::new(),
          Adapter::new(
            CaptureTransport {
              requests: &requests,
              remaining: b"{}",
              fail: false,
            },
            TestRuntime,
          ),
        )
        .unwrap();
        let body = json!({
          "model": config.model_id,
          "messages": [{ "role": "user", "content": "Hello" }],
          "stream": false,
          "vendor_metadata": { "retain": true }
        });
        let mut options = RequestOptions::default();
        if let Some(key) = key {
          options = options.with_header("authorization", format!("Bearer {key}"));
        }
        let response = client
          .execute(Endpoint::ChatCompletions, body.clone(), options)
          .await
          .unwrap();
        response.into_json::<Value>().unwrap();
        let requests = requests.borrow();
        assert_eq!(requests.len(), 1);
        let sent = &requests[0];
        assert_eq!(sent.url, completion_url(&config).unwrap().as_str());
        assert_eq!(serde_json::from_slice::<Value>(&sent.body).unwrap(), body);
        assert_eq!(
          sent
            .headers
            .get("authorization")
            .map(|value| value.as_str()),
          key.map(|_| "Bearer test-key")
        );
      }
    }
  }

  #[tokio::test]
  async fn does_not_retry_recoverable_transport_failures() {
    let config = config("https://example.invalid/v1");
    let requests = RefCell::new(Vec::new());
    let client = Client::new(
      execution_rules(&config).unwrap(),
      Vec::new(),
      Adapter::new(
        CaptureTransport {
          requests: &requests,
          remaining: b"",
          fail: true,
        },
        TestRuntime,
      ),
    )
    .unwrap();
    assert!(client
      .execute(
        Endpoint::ChatCompletions,
        json!({
          "model": config.model_id,
          "messages": [{ "role": "user", "content": "Hello" }],
          "stream": false
        }),
        RequestOptions::default(),
      )
      .await
      .is_err());
    assert_eq!(requests.borrow().len(), 1);
  }
}

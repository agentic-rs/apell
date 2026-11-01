use super::*;
use crate::providers::{device_account, http_client, static_account, DeviceFlowOutcome};
use serde_json::json;
use std::{
  io::{Read, Write},
  net::TcpListener,
  sync::mpsc,
  thread,
  time::{SystemTime, UNIX_EPOCH},
};

fn config(provider_id: &str, base_url: &str) -> LlmConfig {
  LlmConfig {
    provider_id: provider_id.into(),
    base_url: base_url.into(),
    model_id: String::new(),
    connection_id: "test-connection".into(),
  }
}

fn oauth(provider_id: &str) -> AccountConfig {
  device_account(
    provider_id,
    "test-connection",
    DeviceFlowOutcome {
      refresh_token: "refresh-secret".into(),
      access_token: "access-secret".into(),
      access_token_expires_at: SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 3600,
      username: None,
      provider_account_id: Some("provider-account".into()),
    },
  )
  .unwrap()
}

struct Server {
  url: String,
  request: mpsc::Receiver<String>,
  worker: thread::JoinHandle<()>,
}

impl Server {
  fn start(status: &str, extra_headers: &str, body: &str) -> Self {
    Self::with_length(status, extra_headers, body, body.len())
  }

  fn with_length(status: &str, extra_headers: &str, body: &str, content_length: usize) -> Self {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/prefix/v1", listener.local_addr().unwrap());
    let response = format!(
      "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {}\r\n{extra_headers}\r\n{body}",
      content_length
    );
    let (sender, request) = mpsc::channel();
    let worker = thread::spawn(move || {
      let (mut stream, _) = listener.accept().unwrap();
      stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
      stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
      let mut request = Vec::new();
      let mut buffer = [0; 1024];
      while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let count = stream.read(&mut buffer).unwrap();
        assert_ne!(count, 0);
        request.extend_from_slice(&buffer[..count]);
      }
      sender.send(String::from_utf8(request).unwrap()).unwrap();
      // A bounded reader can close early when rejecting an oversized response.
      let _ = stream.write_all(response.as_bytes());
    });
    Self {
      url,
      request,
      worker,
    }
  }

  fn finish(self) -> String {
    let request = self.request.recv_timeout(Duration::from_secs(5)).unwrap();
    self.worker.join().unwrap();
    request
  }
}

#[test]
fn model_lists_normalize_codex_and_compatible_formats_without_catalogue_filtering() {
  let rows = parse_models(
    "custom",
    &json!({"data":[
      {"id":"private-new-model", "name":"Private model"},
      {"id":"private-new-model", "name":"Duplicate"},
      {"id":"z-model"},
      {"id":"invalid\nmodel"},
      {"no_id":true}
    ]}),
  )
  .unwrap();
  assert_eq!(
    rows,
    [
      ProviderModel {
        model_id: "private-new-model".into(),
        name: "Private model".into()
      },
      ProviderModel {
        model_id: "z-model".into(),
        name: "z-model".into()
      },
    ]
  );
  assert_eq!(
    parse_models(
      "codex",
      &json!({"catalog":{"models":[{"slug":"codex-model", "display_name":"Codex model"}]}})
    )
    .unwrap(),
    [ProviderModel {
      model_id: "codex-model".into(),
      name: "Codex model".into()
    }]
  );
  assert_eq!(
    parse_models("codex", &json!(["new-codex-model"])).unwrap()[0].model_id,
    "new-codex-model"
  );
  assert!(
    parse_models("custom", &json!({"error":"private-upstream-error"}))
      .unwrap_err()
      .contains("invalid model list")
  );
  assert!(parse_models("custom", &json!({"data":[]}))
    .unwrap()
    .is_empty());
  assert!(parse_models("custom", &json!({"data":[{"id":""}]})).is_err());
}

#[test]
fn requests_preserve_prefix_and_reject_wrong_account_or_unsafe_destinations() {
  let input = config("custom", "https://example.invalid/compatible/v1");
  let account = static_account("custom", "test-connection", Some("test-secret")).unwrap();
  let (url, headers) = model_request(&input, Some(&account)).unwrap();
  assert_eq!(url.as_str(), "https://example.invalid/compatible/v1/models");
  assert_eq!(
    headers.get("authorization").unwrap().as_str(),
    "Bearer test-secret"
  );
  let mut wrong = account.clone();
  wrong.id = "different-connection".into();
  assert!(model_request(&input, Some(&wrong)).is_err());
  wrong.id = input.connection_id.clone();
  wrong.provider = "deepseek".into();
  assert!(model_request(&input, Some(&wrong)).is_err());
  for url in [
    "http://example.invalid/v1",
    "https://example.invalid/v1?key=secret",
    "https://user:secret@example.invalid/v1",
  ] {
    assert!(model_request(&config("custom", url), Some(&account)).is_err());
  }
  assert!(model_request(
    &config("github-copilot", "https://example.invalid"),
    Some(&oauth("github-copilot"))
  )
  .is_err());
  assert!(model_request(&config("openai", "https://api.openai.com/v1"), None).is_err());
  let (_, headers) = model_request(&input, None).unwrap();
  assert!(headers.get("authorization").is_none());
  for url in [
    "http://localhost:8080/v1",
    "http://127.0.0.2:8080/v1",
    "http://[::1]:8080/v1",
  ] {
    assert!(model_request(&config("custom", url), None).is_ok());
  }
  for (input, expected) in [
    ("http://127.1:8080/v1", "http://127.0.0.1:8080/v1/models"),
    (
      "https://EXAMPLE.invalid/old/../v1",
      "https://example.invalid/v1/models",
    ),
  ] {
    assert_eq!(
      model_request(&config("custom", input), None)
        .unwrap()
        .0
        .as_str(),
      expected
    );
  }
}

#[test]
fn device_provider_model_headers_use_native_identity_without_exposing_refresh_tokens() {
  let account = oauth("codex");
  let (url, headers) = model_request(
    &config("codex", default_base_url("codex").unwrap()),
    Some(&account),
  )
  .unwrap();
  assert_eq!(url.path(), "/backend-api/codex/models");
  let version = headers.get("version").unwrap().as_str();
  assert_eq!(
    url
      .query_pairs()
      .find(|(key, _)| key == "client_version")
      .unwrap()
      .1,
    version
  );
  assert_eq!(
    headers.get("authorization").unwrap().as_str(),
    "Bearer access-secret"
  );
  assert_eq!(
    headers.get("chatgpt-account-id").unwrap().as_str(),
    "provider-account"
  );
  assert!(!headers
    .iter()
    .any(|(_, value)| value.as_str().contains("refresh-secret")));

  let account = oauth("github-copilot");
  let (url, headers) = model_request(
    &config(
      "github-copilot",
      default_base_url("github-copilot").unwrap(),
    ),
    Some(&account),
  )
  .unwrap();
  assert_eq!(url.path(), "/models");
  assert_eq!(
    headers.get("authorization").unwrap().as_str(),
    "Bearer access-secret"
  );
  assert!(headers.get("copilot-integration-id").is_some());
  assert!(headers.get("editor-version").is_some());
}

#[tokio::test]
async fn live_discovery_fetches_selected_prefix_and_returns_public_rows_only() {
  let server = Server::start(
    "200 OK",
    "Content-Type: application/json\r\n",
    r#"{"data":[{"id":"new-model","name":"New model","private_metadata":"do-not-expose"}]}"#,
  );
  let account = static_account("custom", "test-connection", Some("test-secret")).unwrap();
  let result = discover_models(
    &http_client().unwrap(),
    &config("custom", &server.url),
    Some(account),
  )
  .await
  .unwrap();
  assert_eq!(
    serde_json::to_value(&result).unwrap(),
    json!({"models":[{"model_id":"new-model","name":"New model"}],"source":"provider"})
  );
  let request = server.finish();
  assert!(request.starts_with("GET /prefix/v1/models HTTP/1.1\r\n"));
  assert!(request.contains("authorization: Bearer test-secret\r\n"));
}

#[tokio::test]
async fn discovery_rejects_redirects_and_never_echoes_upstream_errors() {
  let http = http_client().unwrap();
  let server = Server::start(
    "302 Found",
    "Location: https://example.invalid/credential-sink\r\n",
    "private-response",
  );
  let result = discover_models(&http, &config("custom", &server.url), None)
    .await
    .unwrap_err();
  assert!(result.contains("redirected"));
  assert!(!result.contains("private-response"));
  server.finish();
  let server = Server::start("401 Unauthorized", "", "private-response access-secret");
  let result = discover_models(&http, &config("custom", &server.url), None)
    .await
    .unwrap_err();
  assert!(result.contains("rejected the credentials"));
  assert!(!result.contains("private-response"));
  server.finish();
}

#[tokio::test]
async fn discovery_bounds_response_size_before_reading_payload() {
  let server = Server::with_length("200 OK", "", "", MAX_MODEL_BYTES + 1);
  let result = discover_models(
    &http_client().unwrap(),
    &config("custom", &server.url),
    None,
  )
  .await
  .unwrap_err();
  assert!(result.contains("too large"));
  server.finish();
}

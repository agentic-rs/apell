use super::*;
use crate::publish::{
  send,
  tests::{fixture, read_request},
};
use std::{io::Write, net::TcpListener, thread};

const RUNNER_TOKEN: &str = "fixture-runner-secret-never-for-store";
const IDENTITY: &str = "eyJhbGciOiJSUzI1NiJ9.e30.c2lnbmF0dXJl";
const SCOPED_TOKEN: &str =
  "apel_ci_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn reply(status: u16, body: &str) -> String {
  format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

fn server(responses: Vec<String>) -> (Url, thread::JoinHandle<Vec<String>>) {
  let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
  let endpoint = Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
  let handle = thread::spawn(move || {
    responses
      .into_iter()
      .map(|response| {
        let (mut stream, _) = listener.accept().unwrap();
        let request = String::from_utf8_lossy(&read_request(&mut stream)).into_owned();
        let _ = stream.write_all(response.as_bytes());
        request
      })
      .collect()
  });
  (endpoint, handle)
}

fn client() -> Client {
  Client::builder()
    .no_proxy()
    .redirect(Policy::none())
    .timeout(Duration::from_secs(5))
    .build()
    .unwrap()
}

fn timestamp() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap()
    .as_secs()
}

#[test]
fn auth_selection_is_explicit_or_prefers_oidc_on_github_actions() {
  assert_eq!(method(None, false, false, false), AuthMethod::Account);
  assert_eq!(method(None, false, false, true), AuthMethod::Token);
  assert_eq!(method(None, false, true, true), AuthMethod::Account);
  assert_eq!(method(None, false, true, false), AuthMethod::Account);
  assert_eq!(method(None, true, true, true), AuthMethod::GithubOidc);
  assert_eq!(
    method(Some(AuthMethod::Token), true, true, true),
    AuthMethod::Token
  );
  assert_eq!(
    method(Some(AuthMethod::Account), true, false, true),
    AuthMethod::Account
  );
  assert_eq!(
    method(Some(AuthMethod::GithubOidc), false, true, true),
    AuthMethod::GithubOidc
  );
  for (flag, expected) in [
    ("account", AuthMethod::Account),
    ("token", AuthMethod::Token),
    ("github-oidc", AuthMethod::GithubOidc),
  ] {
    let values = [
      "sample.apel",
      "--metadata",
      "metadata.json",
      "--store",
      "https://store.example",
      "--auth",
      flag,
    ]
    .map(std::ffi::OsString::from);
    assert_eq!(PublishArgs::parse(&values).unwrap().auth, Some(expected));
  }
  for suffix in [
    vec!["--auth", "unknown"],
    vec!["--auth", "token", "--auth", "github-oidc"],
  ] {
    let mut values = vec![
      "sample.apel",
      "--metadata",
      "metadata.json",
      "--store",
      "https://store.example",
    ];
    values.extend(suffix);
    assert!(PublishArgs::parse(
      &values
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect::<Vec<_>>()
    )
    .is_err());
  }
}

#[test]
fn github_url_is_trusted_and_audience_uses_the_canonical_store_origin() {
  let store = input::store_endpoint("https://STORE.example:443/").unwrap();
  let audience = store_audience(&store).unwrap();
  assert_eq!(audience, "https://store.example");
  let endpoint = identity_endpoint("https://pipelines.actions.githubusercontent.com/job/idtoken?api-version=2.0&audience=wrong&audience=wrong-again", &audience).unwrap();
  assert_eq!(
    endpoint.query_pairs().collect::<Vec<_>>(),
    vec![
      ("api-version".into(), "2.0".into()),
      ("audience".into(), audience.into())
    ]
  );
  for invalid in [
    "http://pipelines.actions.githubusercontent.com/job/idtoken",
    "https://actions.githubusercontent.com/job/idtoken",
    "https://pipelines.actions.githubusercontent.com.example/job/idtoken",
    "https://example.com/job/idtoken",
    "https://pipelines.actions.githubusercontent.com:444/job/idtoken",
    "https://user:secret@pipelines.actions.githubusercontent.com/job/idtoken",
    "https://@pipelines.actions.githubusercontent.com/job/idtoken",
    "https://pipelines.actions.githubusercontent.com/job/idtoken#fragment",
    "https://pipelines.actions.githubusercontent.com/",
    "https://pipelines.actions.githubusercontent.com\\job\\idtoken",
    "https://pipelines.actions.githubusercontent.com/job/idtoken\n",
  ] {
    assert!(
      identity_endpoint(invalid, "https://store.example").is_err(),
      "{invalid}"
    );
  }
  assert!(store_audience(&input::store_endpoint("http://localhost:3000").unwrap()).is_err());
}

#[test]
fn oidc_exchange_and_upload_keep_each_credential_at_its_destination() {
  let (github, github_thread) = server(vec![reply(
    200,
    &serde_json::json!({"value":IDENTITY}).to_string(),
  )]);
  let prepared = fixture(input::store_endpoint("https://store.example").unwrap());
  let result = serde_json::json!({"applet_id":prepared.manifest.applet_id,"version":prepared.manifest.version,"sha256":prepared.sha256,"created":true}).to_string();
  let (store, store_thread) = server(vec![
    reply(
      200,
      &serde_json::json!({"token":SCOPED_TOKEN,"expires_at":timestamp()+300}).to_string(),
    ),
    reply(201, &result),
  ]);
  let endpoint = input::store_endpoint(store.as_str()).unwrap();
  let client = client();
  let identity = request_identity(&client, github, RUNNER_TOKEN).unwrap();
  let token = exchange(&client, &endpoint, &identity, &prepared).unwrap();
  let args = PublishArgs {
    package: "unused.apel".into(),
    metadata: "unused.json".into(),
    icon: None,
    auth: None,
    endpoint,
  };
  assert!(send(&args, &prepared, &token).unwrap().created);
  let github_requests = github_thread.join().unwrap();
  assert!(github_requests[0].contains(RUNNER_TOKEN));
  assert!(!github_requests[0].contains(SCOPED_TOKEN));
  let store_requests = store_thread.join().unwrap();
  assert!(store_requests[0].starts_with("POST /api/v1/auth/github-actions HTTP/1.1"));
  assert!(store_requests[0].contains(&format!("Bearer {IDENTITY}")));
  let body = store_requests[0].split_once("\r\n\r\n").unwrap().1;
  assert_eq!(
    serde_json::from_str::<serde_json::Value>(body).unwrap(),
    serde_json::json!({"applet_id":"fixture","sha256":prepared.sha256})
  );
  assert!(store_requests[1].starts_with("POST /api/v1/publish HTTP/1.1"));
  assert!(store_requests[1].contains(SCOPED_TOKEN));
  assert!(!store_requests[1].contains(IDENTITY));
  assert!(store_requests.iter().all(|r| !r.contains(RUNNER_TOKEN)));
}

#[test]
fn authentication_redirects_never_forward_credentials() {
  let destination = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
  destination.set_nonblocking(true).unwrap();
  let redirect = format!("HTTP/1.1 307 Redirect\r\nLocation: http://{}/receive\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", destination.local_addr().unwrap());
  for exchange_step in [false, true] {
    let (endpoint, handle) = server(vec![redirect.clone()]);
    let error = if exchange_step {
      let prepared = fixture(endpoint.clone());
      exchange(&client(), &endpoint, IDENTITY, &prepared).unwrap_err()
    } else {
      request_identity(&client(), endpoint, RUNNER_TOKEN).unwrap_err()
    };
    assert!(error.contains("HTTP 307"));
    assert!(error.contains("No token fallback"));
    handle.join().unwrap();
    assert_eq!(
      destination.accept().unwrap_err().kind(),
      std::io::ErrorKind::WouldBlock
    );
  }
}

#[test]
fn malformed_authentication_responses_are_bounded_and_do_not_disclose_secrets() {
  for response in [
    reply(401, &format!("{RUNNER_TOKEN} {IDENTITY} {SCOPED_TOKEN}")),
    reply(200, "not JSON"),
    reply(200, r#"{"value":"not a jwt"}"#),
    reply(
      200,
      &serde_json::json!({"value":format!("e30.e30.{}", "a".repeat(MAX_IDENTITY_BYTES))})
        .to_string(),
    ),
    reply(
      200,
      &serde_json::json!({"value":"x".repeat(MAX_AUTH_RESPONSE_BYTES)}).to_string(),
    ),
  ] {
    let (endpoint, handle) = server(vec![response]);
    let error = request_identity(&client(), endpoint, RUNNER_TOKEN).unwrap_err();
    for secret in [RUNNER_TOKEN, IDENTITY, SCOPED_TOKEN] {
      assert!(!error.contains(secret));
    }
    handle.join().unwrap();
  }
  for body in [
    serde_json::json!({"token":SCOPED_TOKEN,"expires_at":timestamp()-1}),
    serde_json::json!({"token":SCOPED_TOKEN,"expires_at":timestamp()+3600}),
    serde_json::json!({"token":"short","expires_at":timestamp()+300}),
    serde_json::json!({"token":format!("{SCOPED_TOKEN}\n"),"expires_at":timestamp()+300}),
    serde_json::json!({"token":"x".repeat(513),"expires_at":timestamp()+300}),
  ] {
    let (endpoint, handle) = server(vec![reply(200, &body.to_string())]);
    let prepared = fixture(endpoint.clone());
    let error = exchange(&client(), &endpoint, IDENTITY, &prepared).unwrap_err();
    assert!(!error.contains(SCOPED_TOKEN));
    assert!(!error.contains(IDENTITY));
    handle.join().unwrap();
  }
}

#[test]
fn later_publish_errors_redact_all_oidc_credentials_and_headers_are_sensitive() {
  let credential = Credential {
    value: SCOPED_TOKEN.into(),
    redactions: vec![IDENTITY.into(), RUNNER_TOKEN.into()],
  };
  let message = credential.redact(format!("Echoed {SCOPED_TOKEN} {IDENTITY} {RUNNER_TOKEN}"));
  assert_eq!(message, "Echoed [redacted] [redacted] [redacted]");
  assert!(bearer(IDENTITY, MAX_IDENTITY_BYTES).unwrap().is_sensitive());
  assert!(bearer("contains whitespace", MAX_IDENTITY_BYTES).is_err());
}

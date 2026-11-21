#![cfg(unix)]

use serde_json::{json, Value};
use std::{
  collections::VecDeque,
  fs,
  io::{Read, Write},
  net::{Ipv4Addr, TcpListener, TcpStream},
  os::unix::fs::{symlink, PermissionsExt},
  path::Path,
  process::{Command, Output},
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
  },
  thread,
  time::Duration,
};

const MANAGEMENT_TOKEN: &str = "fixture-management-token-never-print";
const CLIENT_SECRET: &str = "fixture-client-secret-never-print";
const ISSUER: &str = "https://account.example/oidc";

struct Fixture {
  origin: String,
  stopped: Arc<AtomicBool>,
  handle: thread::JoinHandle<Vec<String>>,
}

impl Fixture {
  fn new(responses: Vec<(u16, Value)>) -> Self {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stopped = Arc::clone(&stopped);
    let handle = thread::spawn(move || {
      let mut responses = VecDeque::from(responses);
      let mut requests = Vec::new();
      while !worker_stopped.load(Ordering::Acquire) {
        let (mut stream, _) = match listener.accept() {
          Ok(connection) => connection,
          Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            thread::sleep(Duration::from_millis(5));
            continue;
          }
          Err(error) => panic!("Fixture listener failed: {error}"),
        };
        stream.set_nonblocking(false).unwrap();
        requests.push(read_request(&mut stream));
        let (status, body) = responses
          .pop_front()
          .unwrap_or((500, json!({"message":"Unexpected request"})));
        let body = if body.is_null() {
          String::new()
        } else {
          body.to_string()
        };
        write!(
          stream,
          "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len(),
        )
        .unwrap();
      }
      requests
    });
    Self {
      origin,
      stopped,
      handle,
    }
  }

  fn finish(self) -> Vec<String> {
    self.stopped.store(true, Ordering::Release);
    self.handle.join().unwrap()
  }
}

fn read_request(stream: &mut TcpStream) -> String {
  stream
    .set_read_timeout(Some(Duration::from_secs(5)))
    .unwrap();
  let mut bytes = Vec::new();
  loop {
    let mut chunk = [0; 4096];
    let count = stream.read(&mut chunk).unwrap();
    assert!(count > 0, "Fixture request was incomplete");
    bytes.extend_from_slice(&chunk[..count]);
    assert!(
      bytes.len() <= 64 * 1024,
      "Fixture request exceeded its limit"
    );
    if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
      let headers = String::from_utf8_lossy(&bytes[..end]);
      let length = headers
        .lines()
        .find_map(|line| {
          let (name, value) = line.split_once(':')?;
          name
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
      if bytes.len() >= end + 4 + length {
        return String::from_utf8(bytes).unwrap();
      }
    }
  }
}

fn cli(root: &Path, endpoint: &str, arguments: &[&str]) -> Output {
  Command::new(env!("CARGO_BIN_EXE_apell"))
    .args(["auth", "app", "register"])
    .args(arguments)
    .env("ACCOUNT_MANAGEMENT_ENDPOINT", endpoint)
    .env("ACCOUNT_MANAGEMENT_TOKEN", MANAGEMENT_TOKEN)
    .env("APELL_CLI_CONFIG_DIR", root.join("store-credentials"))
    .current_dir(root)
    .output()
    .unwrap()
}

fn safe_output(output: &Output) -> (String, String) {
  let stdout = String::from_utf8(output.stdout.clone()).unwrap();
  let stderr = String::from_utf8(output.stderr.clone()).unwrap();
  for secret in [MANAGEMENT_TOKEN, CLIENT_SECRET] {
    assert!(!stdout.contains(secret), "Secret appeared in stdout");
    assert!(!stderr.contains(secret), "Secret appeared in stderr");
  }
  (stdout, stderr)
}

fn registration(app_type: &str, callbacks: &[&str], logout_callbacks: &[&str]) -> Value {
  json!({
    "id": "fixture-client",
    "name": "Fixture application",
    "type": app_type,
    "isThirdParty": true,
    "oidcClientMetadata": {
      "redirectUris": callbacks,
      "postLogoutRedirectUris": logout_callbacks
    }
  })
}

#[test]
fn web_registration_saves_credentials_and_prints_only_public_fields() {
  let directory = tempfile::tempdir().unwrap();
  let fixture = Fixture::new(vec![
    (200, json!({"issuer": ISSUER})),
    (
      201,
      registration(
        "Traditional",
        &[
          "https://app.example/callback",
          "https://app.example/another-callback",
        ],
        &["https://app.example/signed-out"],
      ),
    ),
    (
      200,
      json!([{ "name":"Default secret", "value": CLIENT_SECRET, "expiresAt":null }]),
    ),
    (201, Value::Null),
  ]);
  let output = cli(
    directory.path(),
    &fixture.origin,
    &[
      "--name",
      "Fixture application",
      "--type",
      "web",
      "--redirect-uri",
      "https://app.example/callback",
      "--redirect-uri",
      "https://app.example/another-callback",
      "--post-logout-redirect-uri",
      "https://app.example/signed-out",
      "--output",
      "web-client.json",
      "--json",
    ],
  );
  let requests = fixture.finish();
  let (stdout, stderr) = safe_output(&output);
  assert!(output.status.success(), "{stderr}");
  let summary: Value = serde_json::from_str(&stdout).unwrap();
  assert_eq!(summary["client_id"], "fixture-client");
  assert!(summary.get("client_secret").is_none());
  let path = directory.path().join("web-client.json");
  let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
  assert_eq!(saved["issuer"], ISSUER);
  assert_eq!(saved["client_id"], "fixture-client");
  assert_eq!(saved["client_secret"], CLIENT_SECRET);
  assert_eq!(saved["client_type"], "web");
  assert_eq!(
    saved["redirect_uris"],
    json!([
      "https://app.example/callback",
      "https://app.example/another-callback"
    ])
  );
  assert_eq!(
    saved["post_logout_redirect_uris"],
    json!(["https://app.example/signed-out"])
  );
  assert_eq!(
    fs::metadata(path).unwrap().permissions().mode() & 0o777,
    0o600
  );
  assert!(!directory.path().join("store-credentials").exists());
  assert_eq!(requests.len(), 4);
  assert!(requests[0].starts_with("GET /oidc/.well-known/openid-configuration "));
  assert!(!requests[0].contains(MANAGEMENT_TOKEN));
  assert!(requests[1].starts_with("POST /api/applications "));
  assert!(requests[1]
    .to_ascii_lowercase()
    .contains(&format!("authorization: bearer {MANAGEMENT_TOKEN}")));
  let body: Value = serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
  assert_eq!(body["name"], "Fixture application");
  assert_eq!(body["type"], "Traditional");
  assert_eq!(body["isThirdParty"], true);
  assert_eq!(
    body["oidcClientMetadata"]["redirectUris"],
    saved["redirect_uris"]
  );
  assert_eq!(
    body["oidcClientMetadata"]["postLogoutRedirectUris"],
    saved["post_logout_redirect_uris"]
  );
  assert!(requests[2].starts_with("GET /api/applications/fixture-client/secrets "));
  assert!(requests[3].starts_with("POST /api/applications/fixture-client/user-consent-scopes "));
  let consent: Value = serde_json::from_str(requests[3].split_once("\r\n\r\n").unwrap().1).unwrap();
  assert_eq!(consent, json!({"userScopes":["profile","email"]}));
}

#[test]
fn public_clients_save_exact_callbacks_without_a_client_secret() {
  for (client_type, logto_type, callback) in [
    ("native", "Native", "dev.example.apell:/callback"),
    ("spa", "SPA", "https://app.example/callback"),
  ] {
    let directory = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(vec![
      (200, json!({"issuer": ISSUER})),
      (201, registration(logto_type, &[callback], &[])),
      (201, Value::Null),
    ]);
    let output = cli(
      directory.path(),
      &fixture.origin,
      &[
        "--name",
        "Fixture application",
        "--type",
        client_type,
        "--redirect-uri",
        callback,
        "--output",
        "public-client.json",
        "--json",
      ],
    );
    let requests = fixture.finish();
    let (_, stderr) = safe_output(&output);
    assert!(output.status.success(), "{stderr}");
    let saved: Value =
      serde_json::from_slice(&fs::read(directory.path().join("public-client.json")).unwrap())
        .unwrap();
    assert_eq!(saved["client_type"], client_type);
    assert_eq!(saved["redirect_uris"], json!([callback]));
    assert!(saved.get("client_secret").is_none());
    assert_eq!(requests.len(), 3);
    let body: Value = serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["type"], logto_type);
    assert_eq!(
      body["oidcClientMetadata"]["redirectUris"],
      json!([callback])
    );
    assert!(requests[2].starts_with("POST /api/applications/fixture-client/user-consent-scopes "));
  }
}

#[test]
fn first_party_registration_does_not_add_third_party_consent_scopes() {
  let directory = tempfile::tempdir().unwrap();
  let mut application = registration("Native", &["dev.example.apell:/callback"], &[]);
  application["isThirdParty"] = json!(false);
  let fixture = Fixture::new(vec![(200, json!({"issuer": ISSUER})), (201, application)]);
  let output = cli(
    directory.path(),
    &fixture.origin,
    &[
      "--name",
      "Fixture application",
      "--type",
      "native",
      "--first-party",
      "--redirect-uri",
      "dev.example.apell:/callback",
      "--output",
      "first-party-client.json",
    ],
  );
  let requests = fixture.finish();
  let (_, stderr) = safe_output(&output);
  assert!(output.status.success(), "{stderr}");
  assert_eq!(requests.len(), 2);
  let body: Value = serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
  assert_eq!(body["isThirdParty"], false);
  assert!(directory.path().join("first-party-client.json").is_file());
}

#[test]
fn failed_consent_setup_removes_the_new_application_without_saving_credentials() {
  let directory = tempfile::tempdir().unwrap();
  let fixture = Fixture::new(vec![
    (200, json!({"issuer": ISSUER})),
    (
      201,
      registration("Traditional", &["https://app.example/callback"], &[]),
    ),
    (
      200,
      json!([{ "name":"Default secret", "value": CLIENT_SECRET, "expiresAt":null }]),
    ),
    (
      403,
      json!({"message":format!("{MANAGEMENT_TOKEN} {CLIENT_SECRET}")}),
    ),
    (204, Value::Null),
  ]);
  let output = cli(
    directory.path(),
    &fixture.origin,
    &[
      "--name",
      "Fixture application",
      "--type",
      "web",
      "--redirect-uri",
      "https://app.example/callback",
      "--output",
      "failed-client.json",
    ],
  );
  let requests = fixture.finish();
  let (_, stderr) = safe_output(&output);
  assert!(!output.status.success());
  assert!(stderr.contains("403"));
  assert_eq!(requests.len(), 5);
  assert!(requests[4].starts_with("DELETE /api/applications/fixture-client "));
  assert!(!directory.path().join("failed-client.json").exists());
}

#[test]
fn occupied_output_paths_are_rejected_before_contacting_account() {
  let directory = tempfile::tempdir().unwrap();
  let root = directory.path();
  fs::write(root.join("existing.json"), "keep-existing-credentials").unwrap();
  symlink(root.join("existing.json"), root.join("linked.json")).unwrap();
  for destination in ["existing.json", "linked.json"] {
    let fixture = Fixture::new(Vec::new());
    let output = cli(
      root,
      &fixture.origin,
      &[
        "--name",
        "Fixture application",
        "--type",
        "web",
        "--redirect-uri",
        "https://app.example/callback",
        "--output",
        destination,
      ],
    );
    let requests = fixture.finish();
    safe_output(&output);
    assert!(!output.status.success());
    assert!(
      requests.is_empty(),
      "Account was contacted for an occupied output"
    );
    assert_eq!(
      fs::read_to_string(root.join("existing.json")).unwrap(),
      "keep-existing-credentials"
    );
  }
}

#[test]
fn invalid_callbacks_are_rejected_before_contacting_account() {
  let directory = tempfile::tempdir().unwrap();
  for callback in [
    "http://app.example/callback",
    "https://app.example/callback#fragment",
    "https://user:password@app.example/callback",
    "https://*.example/callback",
  ] {
    let fixture = Fixture::new(Vec::new());
    let output = cli(
      directory.path(),
      &fixture.origin,
      &[
        "--name",
        "Fixture application",
        "--type",
        "web",
        "--redirect-uri",
        callback,
        "--output",
        "client.json",
      ],
    );
    let requests = fixture.finish();
    safe_output(&output);
    assert!(!output.status.success(), "Accepted callback {callback}");
    assert!(
      requests.is_empty(),
      "Account was contacted for an invalid callback"
    );
    assert!(!directory.path().join("client.json").exists());
  }
}

#[test]
fn account_failure_is_sanitized_and_does_not_create_a_credentials_file() {
  let directory = tempfile::tempdir().unwrap();
  let fixture = Fixture::new(vec![
    (200, json!({"issuer": ISSUER})),
    (
      403,
      json!({"message":format!("{MANAGEMENT_TOKEN} {CLIENT_SECRET}")}),
    ),
  ]);
  let output = cli(
    directory.path(),
    &fixture.origin,
    &[
      "--name",
      "Fixture application",
      "--type",
      "web",
      "--redirect-uri",
      "https://app.example/callback",
      "--output",
      "failed-client.json",
    ],
  );
  let requests = fixture.finish();
  let (_, stderr) = safe_output(&output);
  assert!(!output.status.success());
  assert!(stderr.contains("403"));
  assert_eq!(requests.len(), 2);
  assert!(!directory.path().join("failed-client.json").exists());
}

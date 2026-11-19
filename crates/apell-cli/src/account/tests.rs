use super::*;
use args::{Args, Command};
use http::Api;
use reqwest::Method;
use serde_json::{json, Value};
use std::{
  io::{Read, Write},
  net::{TcpListener, TcpStream},
  thread,
  time::Duration,
};

const TOKEN: &str = "apel_user_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DEVICE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn parse(words: &[&str]) -> Result<Args, String> {
  Args::parse(&words.iter().map(OsString::from).collect::<Vec<_>>())
}

fn credential() -> SavedCredential {
  SavedCredential {
    token: TOKEN.into(),
    expires_at: now() + 3600,
    user: User {
      github_id: 42,
      github_login: "fixture-member".into(),
    },
  }
}

fn challenge(store: &Url) -> login::Challenge {
  login::Challenge {
    device_code: DEVICE.into(),
    user_code: "ABCDE-FGHJK".into(),
    verification_uri: store.join("account/cli").unwrap().to_string(),
    verification_uri_complete: store
      .join("account/cli?code=ABCDE-FGHJK")
      .unwrap()
      .to_string(),
    expires_at: now() + 600,
    interval: 3,
  }
}

fn response(status: u16, body: Value) -> String {
  let text = body.to_string();
  format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len())
}

fn request(stream: &mut TcpStream) -> String {
  stream
    .set_read_timeout(Some(Duration::from_secs(5)))
    .unwrap();
  let mut bytes = Vec::new();
  loop {
    let mut chunk = [0; 4096];
    let read = stream.read(&mut chunk).unwrap();
    assert!(read > 0);
    bytes.extend_from_slice(&chunk[..read]);
    if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
      let headers = String::from_utf8_lossy(&bytes[..end]);
      let length = headers
        .lines()
        .find_map(|line| {
          line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(|value| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
      if bytes.len() >= end + 4 + length {
        return String::from_utf8(bytes).unwrap();
      }
    }
  }
}

fn server(responses: Vec<String>) -> (Url, thread::JoinHandle<Vec<String>>) {
  let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
  let store = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
  let handle = thread::spawn(move || {
    responses
      .into_iter()
      .map(|response| {
        let (mut stream, _) = listener.accept().unwrap();
        let request = request(&mut stream);
        let _ = stream.write_all(response.as_bytes());
        request
      })
      .collect()
  });
  (store, handle)
}

#[test]
fn command_parser_accepts_policy_defaults_and_replacements() {
  let args = parse(&[
    "package",
    "oidc",
    "set",
    "sample",
    "--repo",
    "example/applet",
    "--workflow",
    ".github/workflows/publish.yml",
    "--json",
    "--store",
    "https://store.example/",
  ])
  .unwrap();
  assert!(args.json);
  assert_eq!(args.store.unwrap().as_str(), "https://store.example/");
  let Command::OidcSet(id, settings) = args.command else {
    panic!("wrong command");
  };
  assert_eq!(id, "sample");
  assert_eq!(settings.allowed_refs, ["refs/heads/main"]);
  assert_eq!(settings.event_names, ["push", "workflow_dispatch"]);
  let args = parse(&[
    "package",
    "oidc",
    "set",
    "sample",
    "--repo",
    "example/applet",
    "--workflow",
    ".github/workflows/publish.yml",
    "--ref",
    "refs/tags/nightly",
    "--ref",
    "refs/heads/main",
    "--event",
    "release",
    "--environment",
    "production",
  ])
  .unwrap();
  let Command::OidcSet(_, settings) = args.command else {
    panic!("wrong command");
  };
  assert_eq!(
    settings.allowed_refs,
    ["refs/tags/nightly", "refs/heads/main"]
  );
  assert_eq!(settings.event_names, ["release"]);
  assert_eq!(
    serde_json::to_value(settings).unwrap()["environment"],
    "production"
  );
  for words in [
    vec!["whoami", "--no-browser"],
    vec!["login", "--json", "--json"],
    vec!["package", "register", "../escape"],
    vec!["package", "show", "sample?query"],
    vec!["login", "--store", "https://secret@store.example"],
    vec!["login", "--store", "http://store.example"],
    vec!["package", "members", "invite", "sample", "a/b"],
    vec![
      "package",
      "oidc",
      "set",
      "sample",
      "--repo",
      "example/applet",
    ],
    vec!["invitations", "accept", "../escape"],
  ] {
    assert!(parse(&words).is_err(), "{words:?}");
  }
}

#[test]
fn approval_urls_and_expiry_are_bound_to_the_selected_store() {
  let store = store_root("https://store.example").unwrap();
  let valid = challenge(&store);
  valid.validate(&store, now()).unwrap();
  for url in [
    "https://other.example/account/cli?code=ABCDE-FGHJK",
    "https://store.example/account/cli?code=ABCDE-FGHJK&next=https://other.example",
    "https://store.example/account/cli?code=ABCDE-FGHJK#fragment",
  ] {
    let mut invalid = challenge(&store);
    invalid.verification_uri_complete = url.into();
    assert!(invalid.validate(&store, now()).is_err());
  }
  let mut invalid = challenge(&store);
  invalid.device_code = "short".into();
  assert!(invalid.validate(&store, now()).is_err());
  let mut invalid = challenge(&store);
  invalid.expires_at = now() - 1;
  assert!(invalid.validate(&store, now()).is_err());
  let mut invalid = challenge(&store);
  invalid.expires_at = now() + 3600;
  assert!(invalid.validate(&store, now()).is_err());
}

#[test]
fn polling_waits_backs_off_and_returns_only_valid_sessions() {
  let (store, handle) = server(vec![
    response(202, json!({"status":"pending","interval":3})),
    response(429, json!({"error":{"code":"slow_down","message":TOKEN}})),
    response(200, serde_json::to_value(credential()).unwrap()),
  ]);
  let mut waits = Vec::new();
  let result = login::poll(
    &Api::new(&store, None).unwrap(),
    &challenge(&store),
    |duration| waits.push(duration.as_secs()),
  )
  .unwrap();
  assert_eq!(result.token, TOKEN);
  assert_eq!(waits, [3, 3, 8]);
  for request in handle.join().unwrap() {
    assert!(request.starts_with("POST /api/v1/auth/cli/poll HTTP/1.1"));
    assert_eq!(
      serde_json::from_str::<Value>(request.split_once("\r\n\r\n").unwrap().1).unwrap(),
      json!({"device_code":DEVICE})
    );
    assert!(!request.contains(TOKEN));
  }
}

#[test]
fn polling_denial_expiry_and_malformed_credentials_do_not_expose_secrets() {
  for (status, body) in [
    (
      403,
      json!({"error":{"code":"authorization_denied","message":format!("{DEVICE} {TOKEN}")}}),
    ),
    (
      400,
      json!({"error":{"code":"authorization_expired","message":TOKEN}}),
    ),
    (
      200,
      json!({"token":TOKEN,"user":{"github_id":42,"github_login":"fixture"},"expires_at":now()-1}),
    ),
    (
      200,
      json!({"token":TOKEN,"user":{"github_id":42,"github_login":"fixture"},"expires_at":now()+10*24*3600}),
    ),
    (
      200,
      json!({"token":"wrong","user":{"github_id":42,"github_login":"fixture"},"expires_at":now()+3600}),
    ),
  ] {
    let (store, handle) = server(vec![response(status, body)]);
    let error = login::poll(&Api::new(&store, None).unwrap(), &challenge(&store), |_| {})
      .err()
      .unwrap();
    assert!(!error.contains(TOKEN));
    assert!(!error.contains(DEVICE));
    handle.join().unwrap();
  }
}

#[test]
fn account_http_blocks_redirects_and_redacts_echoed_credentials() {
  let destination = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
  destination.set_nonblocking(true).unwrap();
  let redirect = format!("HTTP/1.1 307 Redirect\r\nLocation: http://{}/receive\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", destination.local_addr().unwrap());
  let (store, handle) = server(vec![redirect, response(200, json!({TOKEN: [TOKEN]}))]);
  let api = Api::new(&store, Some(TOKEN)).unwrap();
  assert!(api
    .call(Method::GET, "/api/v1/auth/cli/me", None)
    .unwrap_err()
    .contains("redirect"));
  let result = api.call(Method::GET, "/api/v1/auth/cli/me", None).unwrap();
  assert!(!result.to_string().contains(TOKEN));
  assert_eq!(
    destination.accept().unwrap_err().kind(),
    std::io::ErrorKind::WouldBlock
  );
  assert!(handle
    .join()
    .unwrap()
    .iter()
    .all(|request| request.contains(&format!("Bearer {TOKEN}"))));
}

#[test]
fn account_auth_errors_explain_recovery_without_echoing_server_secrets() {
  for (status, code, recovery) in [
    (401, "sign_in_required", "Run apell login again"),
    (429, "store_busy", "Revoke unused CLI access in My packages"),
    (429, "rate_limited", "Try again in a minute"),
  ] {
    let reply = http::Reply {
      status: reqwest::StatusCode::from_u16(status).unwrap(),
      value: json!({"error":{"code":code,"message":format!("Echoed {TOKEN} {DEVICE}")}}),
    };
    let error = reply.error();
    assert!(error.contains(recovery));
    assert!(error.contains(&format!("HTTP {status}")));
    assert!(!error.contains(TOKEN));
    assert!(!error.contains(DEVICE));
  }
}

#[cfg(unix)]
#[test]
fn file_credentials_are_private_origin_bound_and_removed_on_logout() {
  use std::os::unix::fs::{MetadataExt, PermissionsExt};
  let directory = tempfile::tempdir().unwrap();
  let credentials = CredentialStore::at(directory.path().join("cli"), false).unwrap();
  let store = store_root("https://store.example").unwrap();
  assert!(credentials.default_store().unwrap().is_none());
  credentials.save(&store, &credential()).unwrap();
  assert_eq!(credentials.default_store().unwrap().unwrap(), store);
  assert_eq!(credentials.load(&store).unwrap().token, TOKEN);
  assert!(!credentials
    .contains(&store_root("https://other.example").unwrap())
    .unwrap());
  for entry in std::fs::read_dir(directory.path().join("cli")).unwrap() {
    let entry = entry.unwrap();
    assert_eq!(entry.metadata().unwrap().mode() & 0o777, 0o600);
  }
  let record = directory.path().join("cli").join(format!(
    "{}.json",
    apell_package::digest(store.as_str().as_bytes())
  ));
  let mut value: Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
  value["store"] = "https://other.example/".into();
  std::fs::write(&record, serde_json::to_vec(&value).unwrap()).unwrap();
  assert!(credentials.load(&store).is_err());
  value["store"] = store.as_str().into();
  std::fs::write(&record, serde_json::to_vec(&value).unwrap()).unwrap();
  std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o644)).unwrap();
  assert!(credentials.load(&store).is_err());
  std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
  credentials.remove(&store).unwrap();
  assert!(!credentials.contains(&store).unwrap());
  assert!(!record.exists());
}

#[cfg(unix)]
#[test]
fn credential_storage_rejects_symlinks_and_shared_directories() {
  use std::os::unix::fs::{symlink, PermissionsExt};
  let directory = tempfile::tempdir().unwrap();
  let root = directory.path().join("cli");
  let credentials = CredentialStore::at(root.clone(), false).unwrap();
  symlink(&root, directory.path().join("link")).unwrap();
  assert!(CredentialStore::at(directory.path().join("link"), false).is_err());
  symlink(directory.path().join("missing"), root.join("config.json")).unwrap();
  assert!(credentials.default_store().is_err());
  std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
  assert!(CredentialStore::at(root, false).is_err());
}

#[test]
fn management_commands_use_bearer_and_resolve_maintainers_before_changes() {
  let details = json!({"applet_id":"sample","owner":{"github_id":1,"github_login":"fixture-owner"},"maintainers":[{"github_id":42,"github_login":"fixture-member"}],"publisher":null,"invitations":[]});
  let (store, handle) = server(vec![
    response(201, details.clone()),
    response(200, details.clone()),
    response(200, details.clone()),
    response(200, json!({"ok":true})),
    response(200, details.clone()),
    response(200, json!({"ok":true})),
    response(200, details.clone()),
    response(200, json!({"ok":true})),
    response(200, json!({"ok":true})),
  ]);
  let api = Api::new(&store, Some(TOKEN)).unwrap();
  commands::execute(&Command::Register("sample".into()), &api).unwrap();
  let set = parse(&[
    "package",
    "oidc",
    "set",
    "sample",
    "--repo",
    "example/applet",
    "--workflow",
    ".github/workflows/publish.yml",
  ])
  .unwrap();
  commands::execute(&set.command, &api).unwrap();
  commands::execute(
    &Command::Remove("sample".into(), "FIXTURE-MEMBER".into()),
    &api,
  )
  .unwrap();
  commands::execute(&Command::Transfer("sample".into(), "42".into()), &api).unwrap();
  assert!(commands::execute(
    &Command::Remove("sample".into(), "unrelated-user".into()),
    &api
  )
  .is_err());
  commands::execute(
    &Command::Invite("sample".into(), "new-maintainer".into()),
    &api,
  )
  .unwrap();
  commands::execute(&Command::OidcDisconnect("sample".into()), &api).unwrap();
  let requests = handle.join().unwrap();
  let expected = [
    "POST /api/v1/account/packages ",
    "PUT /api/v1/account/packages/sample/publisher ",
    "GET /api/v1/account/packages/sample ",
    "DELETE /api/v1/account/packages/sample/members/42 ",
    "GET /api/v1/account/packages/sample ",
    "POST /api/v1/account/packages/sample/ownership ",
    "GET /api/v1/account/packages/sample ",
    "POST /api/v1/account/packages/sample/members ",
    "DELETE /api/v1/account/packages/sample/publisher ",
  ];
  for (request, expected) in requests.iter().zip(expected) {
    assert!(request.starts_with(expected), "{request}");
    assert!(request.contains(&format!("Bearer {TOKEN}")));
  }
  let body = |index: usize| {
    serde_json::from_str::<Value>(requests[index].split_once("\r\n\r\n").unwrap().1).unwrap()
  };
  assert_eq!(body(0), json!({"applet_id":"sample"}));
  assert_eq!(body(1)["allowed_refs"], json!(["refs/heads/main"]));
  assert_eq!(body(5), json!({"github_id":42}));
  assert_eq!(body(7), json!({"github_login":"new-maintainer"}));
  assert_eq!(body(8), json!({}));
}

#![cfg(unix)]

use apell_core::bundles::{digest, pack_directory};
use serde_json::{json, Value};
use std::{
  fs,
  io::{Read, Write},
  net::{TcpListener, TcpStream},
  path::Path,
  process::{Command, Output},
  thread,
  time::{Duration, SystemTime, UNIX_EPOCH},
};

const TOKEN: &str = "apel_user_cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const DEVICE: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const LEGACY_TOKEN: &str = "fixture-legacy-admin-token-must-not-be-selected";

fn read_request(stream: &mut TcpStream) -> String {
  stream
    .set_read_timeout(Some(Duration::from_secs(5)))
    .unwrap();
  let mut bytes = Vec::new();
  loop {
    let mut chunk = [0; 4096];
    let count = stream.read(&mut chunk).unwrap();
    assert!(count > 0);
    bytes.extend_from_slice(&chunk[..count]);
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
        return String::from_utf8_lossy(&bytes).into_owned();
      }
    }
  }
}

fn cli(root: &Path, arguments: &[&str]) -> Output {
  Command::new(env!("CARGO_BIN_EXE_apell"))
    .args(arguments)
    .env("APELL_CLI_CONFIG_DIR", root.join("credentials"))
    .env("APELL_STORE_PUBLISH_TOKEN_FILE", root.join("legacy-token"))
    .env_remove("GITHUB_ACTIONS")
    .current_dir(root)
    .output()
    .unwrap()
}

fn safe_output(output: &Output) -> (String, String) {
  let stdout = String::from_utf8(output.stdout.clone()).unwrap();
  let stderr = String::from_utf8(output.stderr.clone()).unwrap();
  for secret in [TOKEN, DEVICE, LEGACY_TOKEN] {
    assert!(!stdout.contains(secret));
    assert!(!stderr.contains(secret));
  }
  (stdout, stderr)
}

#[test]
fn binary_login_remembers_store_publishes_as_account_and_revokes_on_logout() {
  let directory = tempfile::tempdir().unwrap();
  let root = directory.path();
  fs::create_dir(root.join("package")).unwrap();
  fs::write(root.join("package/manifest.json"), br#"{"schema_version":1,"api_version":2,"applet_id":"fixture","name":"Fixture","version":"1.2.0","permissions":[]}"#).unwrap();
  fs::write(root.join("package/index.html"), "<p>Fixture</p>").unwrap();
  let bundle = pack_directory(&root.join("package")).unwrap();
  let sha256 = digest(&bundle);
  fs::write(root.join("fixture.apel"), bundle).unwrap();
  fs::write(
    root.join("metadata.json"),
    br#"{"description":"Fixture applet"}"#,
  )
  .unwrap();
  fs::write(root.join("legacy-token"), LEGACY_TOKEN).unwrap();
  let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
  let store = format!("http://{}", listener.local_addr().unwrap());
  let now = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap()
    .as_secs();
  let user = json!({"github_id":42,"github_login":"fixture-member"});
  let responses = vec![
    (
      "POST /api/v1/auth/cli/start ",
      200,
      json!({"device_code":DEVICE,"user_code":"ABCDE-FGHJK","verification_uri":format!("{store}/account/cli"),"verification_uri_complete":format!("{store}/account/cli?code=ABCDE-FGHJK"),"expires_at":now+600,"interval":3}),
    ),
    (
      "POST /api/v1/auth/cli/poll ",
      200,
      json!({"token":TOKEN,"expires_at":now+3600,"user":user}),
    ),
    (
      "GET /api/v1/auth/cli/me ",
      200,
      json!({"user":user,"expires_at":now+3600}),
    ),
    (
      "POST /api/v1/publish ",
      201,
      json!({"applet_id":"fixture","version":"1.2.0","sha256":sha256,"created":true}),
    ),
    (
      "POST /api/v1/publish ",
      401,
      json!({"error":{"code":"unauthorized","message":format!("Invalid {TOKEN}")}}),
    ),
    ("POST /api/v1/auth/cli/logout ", 200, json!({"ok":true})),
  ];
  let handle = thread::spawn(move || {
    for (index, (prefix, status, body)) in responses.into_iter().enumerate() {
      let (mut stream, _) = listener.accept().unwrap();
      let request = read_request(&mut stream);
      assert!(request.starts_with(prefix));
      assert!(!request.contains(LEGACY_TOKEN));
      if index < 2 {
        assert!(!request.contains(TOKEN));
      } else {
        assert!(request.contains(&format!("Bearer {TOKEN}")));
      }
      if index == 1 {
        let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body, json!({"device_code":DEVICE}));
      }
      let body = body.to_string();
      write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    }
  });
  let output = cli(
    root,
    &["login", "--store", &store, "--no-browser", "--json"],
  );
  let (stdout, stderr) = safe_output(&output);
  assert!(output.status.success(), "{stderr}");
  assert!(stderr.contains("ABCDE-FGHJK"));
  assert_eq!(
    serde_json::from_str::<Value>(&stdout).unwrap()["user"],
    user
  );

  let output = cli(root, &["whoami", "--json"]);
  let (stdout, stderr) = safe_output(&output);
  assert!(output.status.success(), "{stderr}");
  assert_eq!(
    serde_json::from_str::<Value>(&stdout).unwrap()["user"],
    user
  );

  let output = cli(
    root,
    &["publish", "fixture.apel", "--metadata", "metadata.json"],
  );
  let (_, stderr) = safe_output(&output);
  assert!(output.status.success(), "{stderr}");
  // A rejected account session must not retry with the available administrator token.
  let output = cli(
    root,
    &["publish", "fixture.apel", "--metadata", "metadata.json"],
  );
  safe_output(&output);
  assert!(!output.status.success());

  let output = cli(root, &["logout", "--json"]);
  let (stdout, stderr) = safe_output(&output);
  assert!(output.status.success(), "{stderr}");
  assert_eq!(
    serde_json::from_str::<Value>(&stdout).unwrap(),
    json!({"ok":true})
  );
  handle.join().unwrap();
  let files = fs::read_dir(root.join("credentials"))
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap();
  assert_eq!(files.len(), 1);
  assert_eq!(files[0].file_name(), "config.json");
}

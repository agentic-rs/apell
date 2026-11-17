use super::*;
use std::{
  io::Write,
  net::{TcpListener, TcpStream},
  thread,
};
use url::Url;

const TOKEN: &str = "test-only-publishing-token-1234567890";

fn args(values: &[&str]) -> Vec<OsString> {
  values.iter().map(OsString::from).collect()
}

fn fixture(endpoint: Url) -> PreparedPublish {
  let root = tempfile::tempdir().unwrap();
  let package = root.path().join("package");
  std::fs::create_dir(&package).unwrap();
  std::fs::write(package.join("manifest.json"), br#"{"schema_version":1,"api_version":2,"applet_id":"fixture","name":"Fixture","version":"1.2.0","permissions":[]}"#).unwrap();
  std::fs::write(package.join("index.html"), "<p>Fixture</p>").unwrap();
  let file = root.path().join("private-local-file-name.apel");
  std::fs::write(
    &file,
    apell_core::bundles::pack_directory(&package).unwrap(),
  )
  .unwrap();
  let metadata = root.path().join("metadata.json");
  std::fs::write(
    &metadata,
    br#"{"description":"Test applet","release_notes":"First release"}"#,
  )
  .unwrap();
  PreparedPublish::load(&PublishArgs {
    package: file,
    metadata,
    icon: None,
    endpoint,
  })
  .unwrap()
}

fn read_request(stream: &mut TcpStream) -> Vec<u8> {
  stream
    .set_read_timeout(Some(Duration::from_secs(5)))
    .unwrap();
  let mut bytes = Vec::new();
  let mut buffer = [0; 4096];
  loop {
    let count = stream.read(&mut buffer).unwrap();
    assert!(count > 0, "request stopped before headers");
    bytes.extend_from_slice(&buffer[..count]);
    if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
      let headers = std::str::from_utf8(&bytes[..offset]).unwrap();
      let length: usize = headers
        .lines()
        .find_map(|line| {
          let (name, value) = line.split_once(':')?;
          name
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().unwrap())
        })
        .expect("multipart length is known");
      let total = offset + 4 + length;
      while bytes.len() < total {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "request stopped before body");
        bytes.extend_from_slice(&buffer[..count]);
      }
      return bytes;
    }
    assert!(bytes.len() < 64 * 1024, "oversized request headers");
  }
}

#[test]
fn publish_arguments_require_one_package_store_and_metadata() {
  let parsed = PublishArgs::parse(&args(&[
    "sample.apel",
    "--metadata",
    "listing.json",
    "--icon",
    "logo.png",
    "--store",
    "https://store.example/",
  ]))
  .unwrap();
  assert_eq!(
    parsed.endpoint.as_str(),
    "https://store.example/api/v1/publish"
  );
  assert_eq!(parsed.icon.unwrap(), std::path::Path::new("logo.png"));
  for invalid in [
    vec![],
    vec![
      "sample.zip",
      "--store",
      "https://store.example",
      "--metadata",
      "listing.json",
    ],
    vec!["sample.apel", "--metadata", "listing.json"],
    vec!["sample.apel", "--store", "https://store.example"],
    vec![
      "sample.apel",
      "--store",
      "https://store.example",
      "--metadata",
      "listing.json",
      "--icon",
    ],
    vec![
      "sample.apel",
      "--store",
      "https://store.example",
      "--metadata",
      "listing.json",
      "--store",
      "https://other.example",
    ],
    vec![
      "sample.apel",
      "--store",
      "https://store.example",
      "--metadata",
      "listing.json",
      "--token",
      TOKEN,
    ],
    vec!["sample.apel", "--store", "--metadata", "listing.json"],
  ] {
    assert!(PublishArgs::parse(&args(&invalid)).is_err());
  }
}

#[test]
fn store_url_rejects_credentials_non_root_paths_and_unencrypted_remote_hosts() {
  for valid in [
    "https://store.example",
    "https://store.example:8443/",
    "http://127.0.0.1:4321",
    "http://[::1]:4321/",
    "http://localhost:4321",
  ] {
    assert!(input::store_endpoint(valid).is_ok(), "rejected {valid}");
  }
  for invalid in [
    "http://store.example",
    "http://192.0.2.1",
    "https://user:password@store.example",
    "https://@store.example",
    "https://store.example/path",
    "https://store.example/../",
    "https://store.example?token=value",
    "https://store.example/#fragment",
    "https://store.example//",
    "https://store.example\\",
    " https://store.example",
    "https://store.example\n",
    "file:///tmp/store",
  ] {
    assert!(
      input::store_endpoint(invalid).is_err(),
      "accepted invalid store URL"
    );
  }
}

#[test]
fn token_file_is_bounded_and_accepts_only_non_space_printable_ascii() {
  let root = tempfile::tempdir().unwrap();
  let file = root.path().join("token");
  for value in [format!("{TOKEN}\n"), format!("{}\r\n", "x".repeat(512))] {
    std::fs::write(&file, &value).unwrap();
    assert_eq!(
      input::read_token(&file).unwrap(),
      value.trim_end_matches(['\r', '\n'])
    );
  }
  for value in [
    "short".to_owned(),
    "x".repeat(513),
    format!("{TOKEN} "),
    format!("{TOKEN}\t"),
    format!("{TOKEN}\nembedded"),
    "é".repeat(32),
  ] {
    std::fs::write(&file, value).unwrap();
    assert!(input::read_token(&file).is_err());
  }
}

#[test]
fn publication_sends_auth_and_multipart_and_checks_returned_identity() {
  let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
  let endpoint =
    input::store_endpoint(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
  let prepared = fixture(endpoint.clone());
  let body = serde_json::json!({"applet_id":"fixture","version":"1.2.0","sha256":prepared.sha256,"created":true}).to_string();
  let server = thread::spawn(move || {
    let (mut stream, _) = listener.accept().unwrap();
    let request = read_request(&mut stream);
    write!(stream, "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    request
  });
  let args = PublishArgs {
    package: "unused.apel".into(),
    metadata: "unused.json".into(),
    icon: None,
    endpoint,
  };
  let result = send(&args, &prepared, TOKEN).unwrap();
  assert!(result.created);
  let request = server.join().unwrap();
  let text = String::from_utf8_lossy(&request);
  assert!(text.starts_with("POST /api/v1/publish HTTP/1.1\r\n"));
  assert!(text.lines().any(|line| line
    .split_once(':')
    .is_some_and(|(name, value)| name.eq_ignore_ascii_case("authorization")
      && value.trim() == format!("Bearer {TOKEN}"))));
  assert!(text.contains("name=\"package\"; filename=\"applet.apel\""));
  assert!(text.contains("name=\"metadata\"; filename=\"metadata.json\""));
  assert!(!text.contains("private-local-file-name"));
  let wrong = br#"{"applet_id":"another","version":"1.2.0","sha256":"wrong","created":true}"#;
  assert!(decode_response(StatusCode::CREATED, wrong, &prepared, TOKEN).is_err());
}

#[test]
fn publishing_does_not_follow_redirects_or_forward_authorization() {
  let destination = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
  destination.set_nonblocking(true).unwrap();
  let location = format!("http://{}/receive", destination.local_addr().unwrap());
  let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
  let endpoint =
    input::store_endpoint(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
  let prepared = fixture(endpoint.clone());
  let server = thread::spawn(move || {
    let (mut stream, _) = listener.accept().unwrap();
    read_request(&mut stream);
    write!(stream, "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
  });
  let args = PublishArgs {
    package: "unused.apel".into(),
    metadata: "unused.json".into(),
    icon: None,
    endpoint,
  };
  assert!(send(&args, &prepared, TOKEN)
    .unwrap_err()
    .contains("redirect"));
  server.join().unwrap();
  assert_eq!(
    destination.accept().unwrap_err().kind(),
    std::io::ErrorKind::WouldBlock
  );
}

#[test]
fn store_errors_are_bounded_structured_and_redact_tokens() {
  let prepared = fixture(input::store_endpoint("https://store.example").unwrap());
  let response =
    serde_json::json!({"error":{"code":"version_conflict","message":format!("Rejected {TOKEN}")}})
      .to_string();
  let error =
    decode_response(StatusCode::CONFLICT, response.as_bytes(), &prepared, TOKEN).unwrap_err();
  assert!(error.contains("version_conflict"));
  assert!(error.contains("[redacted]"));
  assert!(!error.contains(TOKEN));
  for message in [
    "\u{1b}[31munsafe".to_owned(),
    "line\nline".to_owned(),
    "x".repeat(2_001),
  ] {
    let response = serde_json::json!({"error":{"code":"bad","message":message}}).to_string();
    assert_eq!(
      decode_response(
        StatusCode::BAD_REQUEST,
        response.as_bytes(),
        &prepared,
        TOKEN
      )
      .unwrap_err(),
      "The store rejected publishing (HTTP 400)."
    );
  }
}

#[test]
fn local_validation_rejects_invalid_packages_metadata_and_icons() {
  let root = tempfile::tempdir().unwrap();
  let package = root.path().join("sample.apel");
  let metadata = root.path().join("metadata.json");
  std::fs::write(&package, b"not an archive").unwrap();
  std::fs::write(&metadata, br#"{"description":"Test"}"#).unwrap();
  let args = PublishArgs {
    package,
    metadata,
    icon: None,
    endpoint: input::store_endpoint("https://store.example").unwrap(),
  };
  assert!(PreparedPublish::load(&args).is_err());
  for value in [
    serde_json::json!({"description":""}),
    serde_json::json!({"description":"x".repeat(2_001)}),
    serde_json::json!({"description":"Test","project_url":"http://example.com"}),
    serde_json::json!({"description":"Test","release_notes":"x".repeat(10_001)}),
  ] {
    assert!(serde_json::from_value::<PublishMetadata>(value)
      .unwrap()
      .validate()
      .is_err());
  }
  assert!(input::validate_icon(b"not a PNG").is_err());
}

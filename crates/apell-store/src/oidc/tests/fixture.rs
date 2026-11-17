use super::super::*;
use axum::{
  body::Body, http::Request as HttpRequest, response::IntoResponse, routing::get, Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{EncodingKey, Header};
use rsa::{pkcs1::EncodeRsaPrivateKey, traits::PublicKeyParts, RsaPrivateKey};
use serde_json::{json, Value};
use std::{
  io::{Cursor, Write},
  sync::{
    atomic::{AtomicUsize, Ordering},
    OnceLock,
  },
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

pub struct TestKey {
  pub encoding: EncodingKey,
  pub jwks: Value,
}

pub fn key() -> &'static TestKey {
  static KEY: OnceLock<TestKey> = OnceLock::new();
  KEY.get_or_init(|| {
    let private = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let jwks = json!({"keys":[{"kid":"fixture-key", "kty":"RSA", "alg":"RS256", "use":"sig", "n":URL_SAFE_NO_PAD.encode(private.n().to_bytes_be()), "e":URL_SAFE_NO_PAD.encode(private.e().to_bytes_be())}]});
    TestKey { encoding:EncodingKey::from_rsa_der(private.to_pkcs1_der().unwrap().as_bytes()), jwks }
  })
}

pub fn config() -> OidcConfig {
  OidcConfig {
    audience: "https://store.example".into(),
    publishers: vec![TrustedPublisher {
      applet_id: "sample".into(),
      repository_id: 123,
      repository_owner_id: 456,
      repository: Some("example/project".into()),
      workflow: ".github/workflows/publish.yml".into(),
      allowed_refs: vec!["refs/heads/main".into()],
      event_names: vec!["workflow_dispatch".into()],
      environment: Some("release".into()),
    }],
  }
}

pub fn claims(jti: &str) -> Value {
  json!({"iss":ISSUER, "aud":"https://store.example", "sub":"repo:example/project:environment:release",
    "exp":now()+300,"iat":now(),"nbf":now(),"jti":jti,"repository_id":"123","repository_owner_id":"456",
    "repository":"example/project", "workflow_ref":"example/project/.github/workflows/publish.yml@refs/heads/main",
    "ref":"refs/heads/main", "event_name":"workflow_dispatch", "runner_environment":"github-hosted", "environment":"release"})
}

pub fn signed(claims: &Value) -> String {
  let mut header = Header::new(Algorithm::RS256);
  header.kid = Some("fixture-key".into());
  jsonwebtoken::encode(&header, claims, &key().encoding).unwrap()
}

pub fn exchange_request(jwt: &str, applet_id: &str, sha256: String) -> HttpRequest<Body> {
  HttpRequest::post("/api/v1/auth/github-actions")
    .header(header::CONTENT_TYPE, "application/json")
    .header(header::AUTHORIZATION, format!("Bearer {jwt}"))
    .body(Body::from(
      serde_json::to_vec(&json!({"applet_id":applet_id,"sha256":sha256})).unwrap(),
    ))
    .unwrap()
}

pub struct JwksServer {
  pub root: tempfile::TempDir,
  pub url: String,
  count: Arc<AtomicUsize>,
  response: Arc<Mutex<(StatusCode, Value)>>,
  task: tokio::task::JoinHandle<()>,
}

impl JwksServer {
  pub async fn new(keys: Value) -> Self {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/keys", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let response = Arc::new(Mutex::new((StatusCode::OK, keys)));
    let counted = count.clone();
    let reply = response.clone();
    let router = Router::new().route(
      "/keys",
      get(move || {
        let counted = counted.clone();
        let reply = reply.clone();
        async move {
          counted.fetch_add(1, Ordering::SeqCst);
          let (status, value) = reply.lock().unwrap().clone();
          (
            status,
            [(header::LOCATION, "https://other.example/keys")],
            Json(value),
          )
            .into_response()
        }
      }),
    );
    let task = tokio::spawn(async move {
      axum::serve(listener, router).await.unwrap();
    });
    Self {
      root: tempfile::tempdir().unwrap(),
      url,
      count,
      response,
      task,
    }
  }
  pub fn requests(&self) -> usize {
    self.count.load(Ordering::SeqCst)
  }
  pub fn replace(&self, status: StatusCode, value: Value) {
    *self.response.lock().unwrap() = (status, value);
  }
}
impl Drop for JwksServer {
  fn drop(&mut self) {
    self.task.abort();
  }
}

pub fn package(id: &str, version: &str) -> Vec<u8> {
  let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
  let manifest = serde_json::to_vec(&json!({"schema_version":1,"api_version":2,"applet_id":id,"name":"Sample","version":version,"permissions":[],"network":false})).unwrap();
  for (name, bytes) in [
    ("manifest.json", manifest.as_slice()),
    ("index.html", b"<html>Sample</html>".as_slice()),
  ] {
    zip
      .start_file(
        name,
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
      )
      .unwrap();
    zip.write_all(bytes).unwrap();
  }
  zip.finish().unwrap().into_inner()
}

pub fn upload(token: &str, package: &[u8]) -> HttpRequest<Body> {
  let mut body = b"--oidc-fixture\r\nContent-Disposition: form-data; name=\"metadata\"\r\n\r\n{\"description\":\"Sample applet\",\"release_notes\":\"\"}\r\n--oidc-fixture\r\nContent-Disposition: form-data; name=\"package\"; filename=\"applet.apel\"\r\n\r\n".to_vec();
  body.extend_from_slice(package);
  body.extend_from_slice(b"\r\n--oidc-fixture--\r\n");
  HttpRequest::post("/api/v1/publish")
    .header(header::AUTHORIZATION, format!("Bearer {token}"))
    .header(
      header::CONTENT_TYPE,
      "multipart/form-data; boundary=oidc-fixture",
    )
    .body(Body::from(body))
    .unwrap()
}

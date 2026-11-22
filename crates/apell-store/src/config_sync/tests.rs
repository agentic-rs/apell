use super::*;
use crate::{Config, ConfigSyncConfig};
use axum::{body::Body, response::Response};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use http_body_util::BodyExt;
use jsonwebtoken::{jwk::Jwk, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use tower::ServiceExt;

const PATH: &str = "/api/v1/config-sync/head";
const CREDENTIAL_PATH: &str = "/api/v1/credential-sync/head";
const CLIENT: &str = "fixture-native-account-app";
const AUDIENCE: &str = "https://store.example.invalid/api/v1/config-sync";
const FIRST: &str = "01234567-0123-4012-8012-012345678901";
const SECOND: &str = "01234567-0123-4012-8012-012345678902";
const THIRD: &str = "01234567-0123-4012-8012-012345678903";
const VAULT: &str = "01234567-0123-4012-8012-012345678904";

struct Fixture {
  _directory: tempfile::TempDir,
  issuer: String,
  signer: EncodingKey,
  service: Router,
  task: tokio::task::JoinHandle<()>,
}

impl Fixture {
  async fn new() -> Self {
    let directory = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}/oidc", listener.local_addr().unwrap());
    let discovery = json!({"issuer": issuer, "jwks_uri": format!("{issuer}/jwks")});
    // PKCS#8 prefix for an ephemeral P-384 scalar.
    let mut der = vec![
      0x30, 0x4e, 0x02, 0x01, 0x00, 0x30, 0x10, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02,
      0x01, 0x06, 0x05, 0x2b, 0x81, 0x04, 0x00, 0x22, 0x04, 0x37, 0x30, 0x35, 0x02, 0x01, 0x01,
      0x04, 0x30,
    ];
    let mut scalar = [0; 48];
    getrandom::fill(&mut scalar).unwrap();
    scalar[0] = 1;
    der.extend_from_slice(&scalar);
    let signer = EncodingKey::from_ec_der(&der);
    let mut jwk = Jwk::from_encoding_key(&signer, Algorithm::ES384).unwrap();
    jwk.common.key_id = Some("sync-fixture-key".into());
    let keys = json!({"keys":[jwk]});
    let account = Router::new()
      .route(
        "/oidc/.well-known/openid-configuration",
        get(move || async move { Json(discovery) }),
      )
      .route("/oidc/jwks", get(move || async move { Json(keys) }));
    let task = tokio::spawn(async move { axum::serve(listener, account).await.unwrap() });
    let authentication: ConfigSyncConfig = serde_json::from_value(json!({
      "issuer":issuer,"client_id":CLIENT,"audience":AUDIENCE
    }))
    .unwrap();
    let config = Config {
      listen: "127.0.0.1:0".parse().unwrap(),
      data_dir: directory.path().join("data"),
      web_dir: directory.path().join("web"),
      publish_token: "fixture-administrator-publish-token".into(),
      oidc: None,
      oauth: None,
      account: None,
      config_sync: Some(authentication),
      bootstrap_owner: None,
    };
    Self {
      _directory: directory,
      issuer,
      signer,
      service: crate::router(&config).unwrap(),
      task,
    }
  }

  fn claims(&self, subject: &str) -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({
      "iss": self.issuer, "sub": subject, "aud": AUDIENCE,
      "iat": now, "exp": now + 300, "client_id": CLIENT, "scope": "config:read config:write",
    })
  }

  fn token(&self, claims: &Value) -> String {
    let mut header = Header::new(Algorithm::ES384);
    header.kid = Some("sync-fixture-key".into());
    jsonwebtoken::encode(&header, claims, &self.signer).unwrap()
  }

  async fn request(&self, token: Option<&str>, body: Option<Value>) -> Response {
    self.request_to(PATH, token, body).await
  }

  async fn request_to(&self, path: &str, token: Option<&str>, body: Option<Value>) -> Response {
    let mut request = Request::builder().uri(path);
    if let Some(token) = token {
      request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = match body {
      Some(body) => request
        .method("PUT")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string())),
      None => request.method("GET").body(Body::empty()),
    }
    .unwrap();
    self.service.clone().oneshot(request).await.unwrap()
  }
}

fn sealed_entry(id: &str, flavor: u8) -> Value {
  json!({
    "connection_id":id,
    "nonce":URL_SAFE_NO_PAD.encode([flavor; 12]),
    "ciphertext":URL_SAFE_NO_PAD.encode([flavor; 32]),
  })
}

fn write_credentials(parent: Option<&str>, revision_id: &str, entries: Vec<Value>) -> Value {
  json!({"expected_head":parent,"revision_id":revision_id,"vault_id":VAULT,"entries":entries})
}

#[tokio::test]
async fn encrypted_credential_heads_are_private_atomic_and_idempotent() {
  let fixture = Fixture::new().await;
  let token = |subject| {
    let mut claims = fixture.claims(subject);
    claims["scope"] = json!("credentials:read credentials:write");
    fixture.token(&claims)
  };
  let first = token("fixture-account-one");
  let second = token("fixture-account-two");
  let read = |token| fixture.request_to(CREDENTIAL_PATH, Some(token), None);
  let write = |token, body| fixture.request_to(CREDENTIAL_PATH, Some(token), Some(body));
  assert_eq!(ok_body(read(&first).await).await, json!({"head":null}));
  let entry_a = sealed_entry("connection-a", 1);
  let entry_b = sealed_entry("connection-b", 2);
  let initial = write_credentials(None, FIRST, vec![entry_b.clone(), entry_a.clone()]);
  let created = ok_body(write(&first, initial).await).await;
  assert_eq!(
    created,
    json!({"head":{"revision_id":FIRST,"vault_id":VAULT,"entries":[entry_a,entry_b]}})
  );
  let retry = write_credentials(None, FIRST, vec![entry_a.clone(), entry_b.clone()]);
  assert_eq!(ok_body(write(&first, retry).await).await, created);
  assert_eq!(ok_body(read(&second).await).await, json!({"head":null}));
  assert_eq!(
    write(&second, write_credentials(Some(FIRST), SECOND, vec![]))
      .await
      .status(),
    StatusCode::CONFLICT
  );
  assert_eq!(
    write(&first, write_credentials(None, FIRST, vec![]))
      .await
      .status(),
    StatusCode::CONFLICT
  );

  let (left, right) = tokio::join!(
    write(
      &first,
      write_credentials(Some(FIRST), SECOND, vec![entry_a])
    ),
    write(&first, write_credentials(Some(FIRST), THIRD, vec![entry_b])),
  );
  let shared = match (left.status(), right.status()) {
    (StatusCode::OK, StatusCode::CONFLICT) => ok_body(left).await,
    (StatusCode::CONFLICT, StatusCode::OK) => ok_body(right).await,
    result => panic!("Unexpected concurrent write result {result:?}"),
  };
  assert_eq!(ok_body(read(&first).await).await, shared);
  let current = shared["head"]["revision_id"].as_str().unwrap();
  assert_eq!(
    write(&first, write_credentials(Some(current), FIRST, vec![]))
      .await
      .status(),
    StatusCode::CONFLICT
  );
  assert_eq!(ok_body(read(&second).await).await, json!({"head":null}));
}

#[tokio::test]
async fn credential_scopes_and_bounded_opaque_entries_are_enforced() {
  let fixture = Fixture::new().await;
  let request = |token, body| fixture.request_to(CREDENTIAL_PATH, token, body);
  let unauthenticated = request(None, None).await;
  assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
  assert_eq!(unauthenticated.headers()[header::CACHE_CONTROL], "no-store");

  let mut claims = fixture.claims("fixture-account");
  let metadata_only = fixture.token(&claims);
  assert_eq!(
    request(Some(&metadata_only), None).await.status(),
    StatusCode::FORBIDDEN
  );
  claims["scope"] = json!("credentials:read");
  let read_only = fixture.token(&claims);
  assert_eq!(
    fixture.request(Some(&read_only), None).await.status(),
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    request(
      Some(&read_only),
      Some(write_credentials(None, FIRST, vec![]))
    )
    .await
    .status(),
    StatusCode::FORBIDDEN
  );
  claims["scope"] = json!("credentials:write");
  let write_only = fixture.token(&claims);

  let entry = sealed_entry("fixture-connection", 1);
  let mut injected = entry.clone();
  injected["api_key"] = json!("fixture-private-value");
  let invalid = |field: &str, value: Value| {
    let mut invalid = entry.clone();
    invalid[field] = value;
    invalid
  };
  for entries in [
    vec![injected],
    vec![entry.clone(), entry.clone()],
    vec![invalid("nonce", json!(URL_SAFE_NO_PAD.encode([0; 11])))],
    vec![invalid("ciphertext", json!("a+b/=="))],
    vec![invalid(
      "ciphertext",
      json!(URL_SAFE_NO_PAD.encode([0; 4113])),
    )],
    vec![invalid("connection_id", json!("wrong/path"))],
    (0..32)
      .map(|id| sealed_entry(&format!("connection-{id}"), 1))
      .collect(),
  ] {
    assert_eq!(
      request(
        Some(&write_only),
        Some(write_credentials(None, FIRST, entries))
      )
      .await
      .status(),
      StatusCode::BAD_REQUEST
    );
  }
  let mut invalid_vault = write_credentials(None, FIRST, vec![entry]);
  invalid_vault["vault_id"] = json!("invalid");
  assert_eq!(
    request(Some(&write_only), Some(invalid_vault))
      .await
      .status(),
    StatusCode::BAD_REQUEST
  );
  let empty = ok_body(
    request(
      Some(&write_only),
      Some(write_credentials(None, FIRST, vec![])),
    )
    .await,
  )
  .await;
  assert_eq!(empty["head"]["entries"], json!([]));
  assert_eq!(
    request(Some(&write_only), None).await.status(),
    StatusCode::FORBIDDEN
  );
  assert_eq!(ok_body(request(Some(&read_only), None).await).await, empty);
}

impl Drop for Fixture {
  fn drop(&mut self) {
    self.task.abort();
  }
}

fn write(parent: Option<&str>, revision_id: &str, model: &str) -> Value {
  json!({
    "expected_head":parent,"revision_id":revision_id,"parent_revision_ids":parent.iter().collect::<Vec<_>>(),
    "snapshot":{
      "schema_version":1,"active_connection_id":"provider-example",
      "connections":[{"connection_id":"provider-example","name":"My model","provider_id":"openai","model_id":model}],
    }
  })
}

async fn ok_body(response: Response) -> Value {
  assert_eq!(response.status(), StatusCode::OK);
  assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
  serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn verified_account_heads_are_private_atomic_and_idempotent() {
  let fixture = Fixture::new().await;
  let first = fixture.token(&fixture.claims("fixture-account-one"));
  let second = fixture.token(&fixture.claims("fixture-account-two"));
  assert_eq!(
    ok_body(fixture.request(Some(&first), None).await).await,
    json!({"head":null})
  );
  let initial = write(None, FIRST, "gpt-fixture");
  let created = ok_body(fixture.request(Some(&first), Some(initial.clone())).await).await;
  assert_eq!(created["head"]["revision_id"], FIRST);
  assert_eq!(created["head"]["snapshot"], initial["snapshot"]);
  assert_eq!(created["head"]["parent_revision_ids"], json!([]));
  assert!(
    chrono::DateTime::parse_from_rfc3339(created["head"]["updated_at"].as_str().unwrap()).is_ok()
  );
  assert_eq!(
    ok_body(fixture.request(Some(&first), Some(initial)).await).await,
    created
  );
  assert_eq!(
    ok_body(fixture.request(Some(&second), None).await).await,
    json!({"head":null})
  );
  assert_eq!(
    fixture
      .request(Some(&second), Some(write(Some(FIRST), SECOND, "other")))
      .await
      .status(),
    StatusCode::CONFLICT
  );
  assert_eq!(
    fixture
      .request(Some(&first), Some(write(None, FIRST, "changed")))
      .await
      .status(),
    StatusCode::CONFLICT
  );

  let (left, right) = tokio::join!(
    fixture.request(Some(&first), Some(write(Some(FIRST), SECOND, "left"))),
    fixture.request(Some(&first), Some(write(Some(FIRST), THIRD, "right"))),
  );
  let shared = match (left.status(), right.status()) {
    (StatusCode::OK, StatusCode::CONFLICT) => ok_body(left).await,
    (StatusCode::CONFLICT, StatusCode::OK) => ok_body(right).await,
    result => panic!("Unexpected concurrent write result {result:?}"),
  };
  assert_eq!(
    ok_body(fixture.request(Some(&first), None).await).await,
    shared
  );
  assert_eq!(shared["head"]["parent_revision_ids"], json!([FIRST]));
  let current = shared["head"]["revision_id"].as_str().unwrap();
  assert_eq!(
    fixture
      .request(Some(&first), Some(write(Some(current), FIRST, "reuse")))
      .await
      .status(),
    StatusCode::CONFLICT
  );
}

#[tokio::test]
async fn bearer_audience_scope_and_metadata_are_enforced() {
  let fixture = Fixture::new().await;
  assert_eq!(
    fixture.request(None, None).await.status(),
    StatusCode::UNAUTHORIZED
  );
  let claims = fixture.claims("fixture-account");
  for (field, value) in [
    ("aud", json!(CLIENT)),
    ("iss", json!("https://another.example.invalid/oidc")),
    ("client_id", json!("different-native-client")),
    ("exp", json!(0)),
    ("sub", json!(CLIENT)),
  ] {
    let mut invalid = claims.clone();
    invalid[field] = value;
    let token = fixture.token(&invalid);
    assert_eq!(
      fixture.request(Some(&token), None).await.status(),
      StatusCode::UNAUTHORIZED
    );
  }
  let mut read_only = claims.clone();
  read_only["scope"] = json!("config:read");
  assert_eq!(
    fixture
      .request(
        Some(&fixture.token(&read_only)),
        Some(write(None, FIRST, "x"))
      )
      .await
      .status(),
    StatusCode::FORBIDDEN
  );
  let token = fixture.token(&claims);
  for key in ["base_url", "api_key", "credential", "provider_token"] {
    let mut injected = write(None, FIRST, "x");
    injected["snapshot"]["connections"][0][key] = json!("fixture-private");
    assert_eq!(
      fixture.request(Some(&token), Some(injected)).await.status(),
      StatusCode::BAD_REQUEST
    );
  }
  for (field, value) in [
    ("schema_version", json!(2)),
    ("active_connection_id", json!("missing")),
  ] {
    let mut invalid = write(None, FIRST, "x");
    invalid["snapshot"][field] = value;
    assert_eq!(
      fixture.request(Some(&token), Some(invalid)).await.status(),
      StatusCode::BAD_REQUEST
    );
  }
  assert_eq!(
    ok_body(fixture.request(Some(&token), None).await).await,
    json!({"head":null})
  );
}

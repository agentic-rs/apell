use super::*;
use axum::{body::Body, http::Method};
use http_body_util::BodyExt;
use jsonwebtoken::{jwk::Jwk, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use tower::ServiceExt;

const PATH: &str = "/api/v1/key-vault";
const VAULT_ID: &str = "01234567-0123-4012-8012-012345678901";
const PAIR_ID: &str = "01234567-0123-4012-8012-012345678902";
const CLIENT: &str = "fixture-native-device";
const AUDIENCE: &str = "https://account.example.invalid/api/v1/key-vault";

fn envelope(seed: u8) -> Envelope {
  Envelope {
    nonce: URL_SAFE_NO_PAD.encode([seed; 12]),
    ciphertext: URL_SAFE_NO_PAD.encode([seed; 48]),
  }
}

#[test]
fn an_idempotent_retry_never_extends_pair_expiry() {
  let directory = tempfile::tempdir().unwrap();
  let mut db = database::open(directory.path()).unwrap();
  let owner = Owner {
    issuer: "fixture-issuer".into(),
    subject: "fixture-subject".into(),
  };
  database::create_vault(
    &mut db,
    &owner,
    &Vault {
      vault_id: VAULT_ID.into(),
      recovery: envelope(1),
    },
  )
  .unwrap();
  database::create_pair(&mut db, &owner, PAIR_ID, VAULT_ID, 1000).unwrap();
  database::create_pair(&mut db, &owner, PAIR_ID, VAULT_ID, 1200).unwrap();
  database::publish_pair(&mut db, &owner, PAIR_ID, &envelope(2), 1299).unwrap();
  assert_eq!(
    database::pair(&db, &owner, PAIR_ID, 1300)
      .err()
      .unwrap()
      .status,
    StatusCode::NOT_FOUND
  );
  database::prune_expired(&db, 1300).unwrap();
  let pair_count: i64 = db
    .query_row("SELECT COUNT(*) FROM key_pairs", [], |row| row.get(0))
    .unwrap();
  assert_eq!(pair_count, 0);
  assert!(database::vault(&db, &owner).unwrap().is_some());
}

struct Fixture {
  _directory: tempfile::TempDir,
  server: tokio::task::JoinHandle<()>,
  router: Router,
  issuer: String,
  signer: EncodingKey,
}

impl Fixture {
  async fn new() -> Self {
    Self::new_with_master(None).await
  }

  async fn new_with_master(master_key: Option<managed::Key>) -> Self {
    let directory = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
      .await
      .unwrap();
    let issuer = format!("http://{}/oidc", listener.local_addr().unwrap());
    let discovery = json!({"issuer":issuer,"jwks_uri":format!("{issuer}/jwks")});
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
    jwk.common.key_id = Some("fixture-relay-key".into());
    let keys = json!({"keys":[jwk]});
    let idp = Router::new()
      .route(
        "/oidc/.well-known/openid-configuration",
        get(move || async move { Json(discovery) }),
      )
      .route("/oidc/jwks", get(move || async move { Json(keys) }));
    let listen = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, idp).await.unwrap() });
    let authentication =
      serde_json::from_value(json!({"issuer":issuer,"client_id":CLIENT,"audience":AUDIENCE}))
        .unwrap();
    let router = Relay::open(Config {
      listen,
      data_dir: directory.path().join("key-relay"),
      authentication,
      master_key,
      recovery: None,
    })
    .unwrap()
    .router();
    Self {
      _directory: directory,
      server,
      router,
      issuer,
      signer,
    }
  }

  fn token(&self, subject: &str, scope: &str, audience: &str) -> String {
    let issued = now();
    let claims = json!({
      "iss":self.issuer,"sub":subject,"aud":audience,"iat":issued,"exp":issued+300,
      "client_id":CLIENT,"scope":scope,
    });
    let mut header = Header::new(Algorithm::ES384);
    header.kid = Some("fixture-relay-key".into());
    jsonwebtoken::encode(&header, &claims, &self.signer).unwrap()
  }

  async fn request(
    &self,
    method: Method,
    path: &str,
    token: Option<&str>,
    payload: Option<Value>,
  ) -> Response {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
      request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = match payload {
      Some(value) => request
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(value.to_string())),
      None => request.body(Body::empty()),
    }
    .unwrap();
    let response = self.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    response
  }
}

impl Drop for Fixture {
  fn drop(&mut self) {
    self.server.abort();
  }
}

async fn read(response: Response) -> Value {
  assert_eq!(response.status(), StatusCode::OK);
  serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn managed_pairing_uses_the_same_code_with_independent_approval_and_retrieval_proofs() {
  const MANAGED: &str = "/api/v1/key-vault/managed";
  let fixture = Fixture::new_with_master(Some(zeroize::Zeroizing::new([19; 32]))).await;
  let read_token = fixture.token("fixture-owner", READ, AUDIENCE);
  let write_token = fixture.token("fixture-owner", WRITE, AUDIENCE);
  let other_token = fixture.token("other-fixture-owner", READ, AUDIENCE);
  let key = [9; 32];
  let request_secret = URL_SAFE_NO_PAD.encode([12; 32]);
  let signing_key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key);
  let proof = |transcript: String| {
    URL_SAFE_NO_PAD.encode(ring::hmac::sign(&signing_key, transcript.as_bytes()).as_ref())
  };
  assert_eq!(
    read(
      fixture
        .request(Method::GET, MANAGED, Some(&read_token), None)
        .await
    )
    .await,
    json!({ "vault_id": null, "managed": false })
  );
  let vault = json!({ "vault_id": VAULT_ID, "vault_key": URL_SAFE_NO_PAD.encode(key) });
  for token in [&read_token, &write_token, &write_token] {
    let status = fixture
      .request(Method::PUT, MANAGED, Some(token), Some(vault.clone()))
      .await
      .status();
    assert_eq!(
      status,
      if token == &read_token {
        StatusCode::FORBIDDEN
      } else {
        StatusCode::NO_CONTENT
      }
    );
  }

  let pair = format!("{MANAGED}/pairs/{PAIR_ID}");
  let started = read(
    fixture
      .request(
        Method::PUT,
        &pair,
        Some(&write_token),
        Some(json!({ "vault_id": VAULT_ID, "request_secret": request_secret })),
      )
      .await,
  )
  .await;
  let code = started["code"].as_str().unwrap();
  assert_eq!(code.len(), 6);
  assert!(code.bytes().all(|byte| byte.is_ascii_digit()));
  let list = read(
    fixture
      .request(
        Method::POST,
        &format!("{MANAGED}/pairs/list"),
        Some(&read_token),
        Some(json!({
          "vault_id": VAULT_ID,
          "proof": proof(format!("apell-key-relay-v2\nlist\n{VAULT_ID}\n")),
        })),
      )
      .await,
  )
  .await;
  assert_eq!(list["pairs"], json!([started]));
  let receipt = json!({ "request_secret": request_secret });
  let finish = format!("{pair}/finish");
  let first = read(
    fixture
      .request(
        Method::POST,
        &finish,
        Some(&read_token),
        Some(receipt.clone()),
      )
      .await,
  )
  .await;
  assert_eq!(first, json!({ "vault_key": null }));
  let approval_nonce = started["approval_nonce"].as_str().unwrap();
  let approve = format!("{pair}/approve");
  assert_eq!(
    fixture
      .request(
        Method::POST,
        &approve,
        Some(&write_token),
        Some(json!({ "vault_id": VAULT_ID, "proof": URL_SAFE_NO_PAD.encode([0; 32]) }))
      )
      .await
      .status(),
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    fixture.request(
      Method::POST, &approve, Some(&write_token),
      Some(json!({
        "vault_id": VAULT_ID,
        "proof": proof(format!("apell-key-relay-v2\napprove\n{VAULT_ID}\n{PAIR_ID}\n{approval_nonce}")),
      }))
    ).await.status(),
    StatusCode::NO_CONTENT
  );
  assert_eq!(
    fixture
      .request(
        Method::POST,
        &finish,
        Some(&other_token),
        Some(receipt.clone())
      )
      .await
      .status(),
    StatusCode::NOT_FOUND
  );
  assert_eq!(
    read(
      fixture
        .request(
          Method::POST,
          &finish,
          Some(&read_token),
          Some(receipt.clone())
        )
        .await
    )
    .await,
    json!({ "vault_key": URL_SAFE_NO_PAD.encode(key) }),
  );
  assert_eq!(
    fixture
      .request(
        Method::POST,
        &format!("{pair}/consume"),
        Some(&write_token),
        Some(receipt.clone())
      )
      .await
      .status(),
    StatusCode::NO_CONTENT
  );
  assert_eq!(
    fixture
      .request(Method::POST, &finish, Some(&read_token), Some(receipt))
      .await
      .status(),
    StatusCode::NOT_FOUND
  );
}

#[tokio::test]
async fn requires_the_relay_audience_and_separate_operation_scopes() {
  let fixture = Fixture::new().await;
  let readonly = fixture.token("fixture-owner", READ, AUDIENCE);
  let wrong = fixture.token(
    "fixture-owner",
    READ,
    "https://store.example.invalid/config",
  );
  for token in [None, Some(wrong.as_str())] {
    assert_eq!(
      fixture
        .request(Method::GET, PATH, token, None)
        .await
        .status(),
      StatusCode::UNAUTHORIZED
    );
  }
  let vault = json!({"vault_id":VAULT_ID,"recovery":envelope(1)});
  let forbidden = fixture
    .request(Method::PUT, PATH, Some(&readonly), Some(vault))
    .await;
  assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
  assert_eq!(
    read(
      fixture
        .request(Method::GET, PATH, Some(&readonly), None)
        .await
    )
    .await,
    json!({"vault":null})
  );
}

#[tokio::test]
async fn vault_and_pairing_are_private_idempotent_and_immutable() {
  let fixture = Fixture::new().await;
  let scopes = format!("{READ} {WRITE}");
  let active = fixture.token("fixture-owner", &scopes, AUDIENCE);
  let other = fixture.token("fixture-other-owner", &scopes, AUDIENCE);
  let vault = json!({"vault_id":VAULT_ID,"recovery":envelope(3)});
  let malformed =
    json!({"vault_id":VAULT_ID,"recovery":{"nonce":"invalid","ciphertext":"invalid"}});
  assert_eq!(
    fixture
      .request(Method::PUT, PATH, Some(&active), Some(malformed))
      .await
      .status(),
    StatusCode::BAD_REQUEST
  );
  for _ in 0..2 {
    assert_eq!(
      fixture
        .request(Method::PUT, PATH, Some(&active), Some(vault.clone()))
        .await
        .status(),
      StatusCode::NO_CONTENT
    );
  }
  let replacement = json!({"vault_id":VAULT_ID,"recovery":envelope(4)});
  assert_eq!(
    fixture
      .request(Method::PUT, PATH, Some(&active), Some(replacement))
      .await
      .status(),
    StatusCode::CONFLICT
  );
  assert_eq!(
    read(
      fixture
        .request(Method::GET, PATH, Some(&active), None)
        .await
    )
    .await,
    json!({"vault":vault})
  );
  assert_eq!(
    read(fixture.request(Method::GET, PATH, Some(&other), None).await).await,
    json!({"vault":null})
  );

  let pair = format!("{PATH}/pairs/{PAIR_ID}");
  let publication = format!("{pair}/envelope");
  let receipt = json!({"vault_id":VAULT_ID});
  assert_eq!(
    fixture
      .request(Method::PUT, &pair, Some(&active), Some(receipt.clone()))
      .await
      .status(),
    StatusCode::NO_CONTENT
  );
  assert_eq!(
    read(
      fixture
        .request(Method::GET, &pair, Some(&active), None)
        .await
    )
    .await,
    json!({"vault_id":VAULT_ID,"envelope":null})
  );
  assert_eq!(
    fixture
      .request(Method::GET, &pair, Some(&other), None)
      .await
      .status(),
    StatusCode::NOT_FOUND
  );
  let sealed = json!(envelope(5));
  assert_eq!(
    fixture
      .request(
        Method::PUT,
        &publication,
        Some(&other),
        Some(sealed.clone())
      )
      .await
      .status(),
    StatusCode::NOT_FOUND
  );
  for _ in 0..2 {
    assert_eq!(
      fixture
        .request(
          Method::PUT,
          &publication,
          Some(&active),
          Some(sealed.clone())
        )
        .await
        .status(),
      StatusCode::NO_CONTENT
    );
  }
  assert_eq!(
    fixture
      .request(
        Method::PUT,
        &publication,
        Some(&active),
        Some(json!(envelope(6)))
      )
      .await
      .status(),
    StatusCode::CONFLICT
  );
  assert_eq!(
    fixture
      .request(Method::DELETE, &pair, Some(&other), None)
      .await
      .status(),
    StatusCode::NO_CONTENT
  );
  assert_eq!(
    read(
      fixture
        .request(Method::GET, &pair, Some(&active), None)
        .await
    )
    .await,
    json!({"vault_id":VAULT_ID,"envelope":sealed})
  );
  assert_eq!(
    fixture
      .request(Method::DELETE, &pair, Some(&active), None)
      .await
      .status(),
    StatusCode::NO_CONTENT
  );
  assert_eq!(
    fixture
      .request(Method::GET, &pair, Some(&active), None)
      .await
      .status(),
    StatusCode::NOT_FOUND
  );
  assert_eq!(
    fixture
      .request(Method::PUT, &pair, Some(&active), Some(receipt))
      .await
      .status(),
    StatusCode::NOT_FOUND
  );
}

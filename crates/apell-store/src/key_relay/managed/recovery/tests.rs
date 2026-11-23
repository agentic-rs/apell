use super::*;
use axum::{
  body::Body,
  extract::Form,
  http::{HeaderMap, Method},
};
use http_body_util::BodyExt;
use jsonwebtoken::{jwk::Jwk, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use url::Url;

const NATIVE_CLIENT: &str = "fixture-native-client";
const WEB_CLIENT: &str = "fixture-recovery-web-client";
const WEB_SECRET: &str = "fixture-recovery-web-secret";
const SUBJECT: &str = "fixture-account-subject";
const AUDIENCE: &str = "https://account.example.invalid/api/v1/key-vault";
const BASE: &str = "/api/v1/key-vault/managed";
const VAULT: &str = "12345678-1234-4567-8901-123456789012";

struct BrowserGrant {
  nonce: String,
  pkce: String,
  subject: String,
}

#[tokio::test]
async fn fresh_browser_approval_releases_vault_once_to_the_requesting_native_client() {
  let directory = tempfile::tempdir().unwrap();
  let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
    .await
    .unwrap();
  let origin = format!("http://{}", listener.local_addr().unwrap());
  let issuer = format!("{origin}/oidc");
  let discovery = json!({
    "issuer": issuer,
    "authorization_endpoint": format!("{issuer}/auth"),
    "token_endpoint": format!("{issuer}/token"),
    "jwks_uri": format!("{issuer}/jwks"),
    "userinfo_endpoint": format!("{issuer}/me"),
  });
  let (signer, jwks) = signing_keys();
  let grant: Arc<Mutex<Option<BrowserGrant>>> = Arc::default();
  let mock = Router::new()
    .route(
      "/oidc/.well-known/openid-configuration",
      get(move || async move { Json(discovery) }),
    )
    .route("/oidc/jwks", get(move || async move { Json(jwks) }))
    .route(
      "/oidc/token",
      post({
        let issuer = issuer.clone();
        let signer = signer.clone();
        let grant = grant.clone();
        let origin = origin.clone();
        move |headers: HeaderMap, Form(form): Form<std::collections::HashMap<String, String>>| async move {
          let grant = grant.lock().unwrap().take().unwrap();
          let basic = base64::engine::general_purpose::STANDARD.encode(format!("{WEB_CLIENT}:{WEB_SECRET}"));
          assert_eq!(headers[header::AUTHORIZATION], format!("Basic {basic}"));
          assert_eq!(form["grant_type"], "authorization_code");
          assert_eq!(form["redirect_uri"], format!("{origin}{CALLBACK}"));
          assert_eq!(form["code"], "fixture-browser-code");
          let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes()));
          assert_eq!(challenge, grant.pkce);
          let time = now();
          let id_token = encode(
            &signer,
            json!({
              "iss": issuer, "sub": grant.subject, "aud": WEB_CLIENT, "iat": time,
              "exp": time + 120, "auth_time": time, "acr": "urn:logto:acr:1fa",
              "nonce": grant.nonce,
            }),
          );
          Json(json!({"id_token": id_token, "access_token":"fixture-access-token", "token_type":"Bearer"}))
        }
      }),
    );
  let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
  let authentication = serde_json::from_value(json!({
    "issuer": issuer,
    "audience": AUDIENCE,
    "client_id": NATIVE_CLIENT,
  }))
  .unwrap();
  let relay = Relay::open(crate::key_relay::Config {
    listen: "127.0.0.1:0".parse().unwrap(),
    data_dir: directory.path().join("relay"),
    authentication,
    master_key: Some(Zeroizing::new([3; 32])),
    recovery: Some(Config {
      client_id: WEB_CLIENT.into(),
      client_secret: WEB_SECRET.into(),
    }),
  })
  .unwrap()
  .router();
  let native_token = |subject| {
    let time = now();
    encode(
      &signer,
      json!({
        "iss": issuer, "sub": subject, "aud": AUDIENCE, "iat": time, "exp": time + 120,
        "client_id": NATIVE_CLIENT, "scope": "key_exchange:read key_exchange:write",
      }),
    )
  };
  let native = native_token(SUBJECT);
  let other = native_token("different-account");
  let vault_key = URL_SAFE_NO_PAD.encode([7; 32]);
  let create_response = request(
    &relay,
    Method::PUT,
    BASE,
    Some(&native),
    Some(json!({"vault_id": VAULT, "vault_key": vault_key})),
  )
  .await;
  assert!(create_response.status().is_success());

  let local_secret = [9u8; 32];
  let finish_body = json!({"request_secret": URL_SAFE_NO_PAD.encode(local_secret)});
  for subject in ["different-account", SUBJECT] {
    let start_response = request(
      &relay,
      Method::POST,
      &format!("{BASE}/recovery/start"),
      Some(&native),
      Some(json!({
        "vault_id": VAULT, "purpose":"recover",
        "request_secret_hash": URL_SAFE_NO_PAD.encode(Sha256::digest(local_secret)),
      })),
    )
    .await;
    assert_eq!(start_response.status(), StatusCode::OK);
    let start = body(start_response).await;
    let id = start["recovery_id"].as_str().unwrap();
    let finish_url = format!("{BASE}/recovery/{id}/finish");

    let pending = request(
      &relay,
      Method::POST,
      &finish_url,
      Some(&native),
      Some(finish_body.clone()),
    )
    .await;
    assert_eq!(body(pending).await, json!({"state":"pending"}));
    let url = Url::parse(start["authorization_url"].as_str().unwrap()).unwrap();
    assert_eq!(url.path(), "/oidc/auth");
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(params["client_id"], WEB_CLIENT);
    assert_eq!(params["max_age"], "0");
    assert_eq!(params["acr_values"], "urn:logto:acr:1fa");
    assert_eq!(params["code_challenge_method"], "S256");
    *grant.lock().unwrap() = Some(BrowserGrant {
      nonce: params["nonce"].clone(),
      pkce: params["code_challenge"].clone(),
      subject: subject.into(),
    });
    let callback_uri = format!(
      "{CALLBACK}?state={}&code=fixture-browser-code",
      params["state"]
    );
    let verified = request(&relay, Method::GET, &callback_uri, None, None).await;
    if subject != SUBJECT {
      assert_eq!(verified.status(), StatusCode::BAD_REQUEST);
      let finish = request(
        &relay,
        Method::POST,
        &finish_url,
        Some(&native),
        Some(finish_body.clone()),
      )
      .await;
      assert_eq!(finish.status(), StatusCode::FORBIDDEN);
      continue;
    }
    assert_eq!(verified.status(), StatusCode::OK);
    assert_eq!(verified.headers()[header::CACHE_CONTROL], "no-store");
    let wrong_account = request(
      &relay,
      Method::POST,
      &finish_url,
      Some(&other),
      Some(finish_body.clone()),
    )
    .await;
    assert_eq!(wrong_account.status(), StatusCode::NOT_FOUND);
    let wrong_secret = request(
      &relay,
      Method::POST,
      &finish_url,
      Some(&native),
      Some(json!({"request_secret": URL_SAFE_NO_PAD.encode([10; 32])})),
    )
    .await;
    assert_eq!(wrong_secret.status(), StatusCode::NOT_FOUND);
    let finish = request(
      &relay,
      Method::POST,
      &finish_url,
      Some(&native),
      Some(finish_body.clone()),
    )
    .await;
    assert_eq!(finish.status(), StatusCode::OK);
    assert_eq!(finish.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
      body(finish).await,
      json!({"state":"complete", "vault_key":vault_key})
    );
    let replay = request(
      &relay,
      Method::POST,
      &finish_url,
      Some(&native),
      Some(finish_body.clone()),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::NOT_FOUND);
  }
  server.abort();
}

async fn request(
  relay: &Router,
  method: Method,
  path: &str,
  token: Option<&str>,
  data: Option<Value>,
) -> Response {
  let mut request = Request::builder().method(method).uri(path);
  if let Some(token) = token {
    request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
  }
  let body = if let Some(data) = data {
    request = request.header(header::CONTENT_TYPE, "application/json");
    Body::from(data.to_string())
  } else {
    Body::empty()
  };
  relay
    .clone()
    .oneshot(request.body(body).unwrap())
    .await
    .unwrap()
}

async fn body(response: Response) -> Value {
  let bytes = response.into_body().collect().await.unwrap().to_bytes();
  serde_json::from_slice(&bytes).unwrap()
}

fn encode(signer: &EncodingKey, claims: Value) -> String {
  let mut header = Header::new(Algorithm::ES384);
  header.kid = Some("fixture-account-key".into());
  jsonwebtoken::encode(&header, &claims, signer).unwrap()
}

fn signing_keys() -> (EncodingKey, Value) {
  // PKCS#8 prefix for an ephemeral P-384 scalar.
  let mut der = vec![
    0x30, 0x4e, 0x02, 0x01, 0x00, 0x30, 0x10, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01,
    0x06, 0x05, 0x2b, 0x81, 0x04, 0x00, 0x22, 0x04, 0x37, 0x30, 0x35, 0x02, 0x01, 0x01, 0x04, 0x30,
  ];
  let mut scalar = [0; 48];
  getrandom::fill(&mut scalar).unwrap();
  scalar[0] = 1;
  der.extend_from_slice(&scalar);
  let signer = EncodingKey::from_ec_der(&der);
  let mut jwk = Jwk::from_encoding_key(&signer, Algorithm::ES384).unwrap();
  jwk.common.key_id = Some("fixture-account-key".into());
  (signer, json!({"keys":[jwk]}))
}

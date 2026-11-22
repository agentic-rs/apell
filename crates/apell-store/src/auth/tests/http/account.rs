use super::{fixture, json_body, request};
use crate::auth::{now, AccountConfig};
use axum::{
  body::Body,
  extract::Form,
  http::{header, HeaderMap, Request, StatusCode},
  response::Response,
  routing::{get, post},
  Json, Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rsa::{pkcs1::EncodeRsaPrivateKey, traits::PublicKeyParts, RsaPrivateKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
  collections::HashMap,
  path::Path,
  sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex, OnceLock,
  },
};
use tower::ServiceExt;
use url::Url;

const CLIENT_ID: &str = "fixture-account-client";
const CLIENT_SECRET: &str = "fixture-account-secret";
const SUBJECT: &str = "store-owner-in-account";
const CALLBACK: &str = "https://store.example.invalid/api/v1/auth/account/callback";
const ACCESS_TOKEN: &str = "fixture-account-access-token";
const SESSION_PREFIX: &str = "__Host-apell_session=";

struct SigningKey {
  encoding: EncodingKey,
  jwks: Value,
}

fn signing_key() -> &'static SigningKey {
  static KEY: OnceLock<SigningKey> = OnceLock::new();
  KEY.get_or_init(|| {
    let private = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    SigningKey {
      encoding: EncodingKey::from_rsa_der(private.to_pkcs1_der().unwrap().as_bytes()),
      jwks: json!({"keys": [{
        "kid": "account-fixture-key",
        "kty": "RSA",
        "alg": "RS256",
        "use": "sig",
        "n": URL_SAFE_NO_PAD.encode(private.n().to_bytes_be()),
        "e": URL_SAFE_NO_PAD.encode(private.e().to_bytes_be()),
      }]}),
    }
  })
}

fn identity_token(issuer: &str, nonce: &str) -> String {
  let mut header = Header::new(Algorithm::RS256);
  header.kid = Some("account-fixture-key".into());
  jsonwebtoken::encode(
    &header,
    &json!({
      "iss": issuer, "sub": SUBJECT, "aud": CLIENT_ID,
      "iat": now(), "exp": now() + 300, "nonce": nonce,
    }),
    &signing_key().encoding,
  )
  .unwrap()
}

struct Authorization {
  challenge: String,
  nonce: String,
}

struct MockAccount {
  origin: String,
  issuer: String,
  authorization: Arc<Mutex<Option<Authorization>>>,
  userinfo_requests: Arc<AtomicUsize>,
  _server: super::super::Server,
}

impl MockAccount {
  async fn new(identities: Value) -> Self {
    signing_key();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let issuer = format!("{origin}/oidc");
    let discovery = json!({
      "issuer": issuer,
      "authorization_endpoint": format!("{issuer}/auth"),
      "token_endpoint": format!("{issuer}/token"),
      "jwks_uri": format!("{issuer}/jwks"),
      "userinfo_endpoint": format!("{issuer}/me"),
    });
    let authorization = Arc::new(Mutex::new(None::<Authorization>));
    let expected = authorization.clone();
    let userinfo_requests = Arc::new(AtomicUsize::new(0));
    let requests = userinfo_requests.clone();
    let token_issuer = issuer.clone();
    let account = Router::new()
      .route(
        "/oidc/.well-known/openid-configuration",
        get(move || async move { Json(discovery) }),
      )
      .route(
        "/oidc/jwks",
        get(|| async { Json(signing_key().jwks.clone()) }),
      )
      .route(
        "/oidc/token",
        post(
          move |headers: HeaderMap, Form(form): Form<HashMap<String, String>>| {
            let expected = expected.clone();
            async move {
              use base64::engine::general_purpose::STANDARD;
              let fields = expected.lock().unwrap().take().unwrap();
              assert_eq!(
                headers[header::AUTHORIZATION],
                format!(
                  "Basic {}",
                  STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
                )
              );
              assert_eq!(form["grant_type"], "authorization_code");
              assert_eq!(form["redirect_uri"], CALLBACK);
              assert_eq!(form["code"], "fixture-code");
              assert_eq!(
                URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
                fields.challenge
              );
              Json(json!({
                "id_token": identity_token(&token_issuer, &fields.nonce),
                "access_token": ACCESS_TOKEN,
                "token_type": "Bearer",
              }))
            }
          },
        ),
      )
      .route(
        "/oidc/me",
        get(move |headers: HeaderMap| async move {
          requests.fetch_add(1, Ordering::Relaxed);
          assert_eq!(
            headers[header::AUTHORIZATION],
            format!("Bearer {ACCESS_TOKEN}")
          );
          Json(json!({
            "sub": SUBJECT,
            "preferred_username": "example-user",
            "identities": identities,
          }))
        }),
      );
    let server = super::super::Server(tokio::spawn(async move {
      axum::serve(listener, account).await.unwrap();
    }));
    Self {
      origin,
      issuer,
      authorization,
      userinfo_requests,
      _server: server,
    }
  }

  fn router(&self, directory: &Path) -> Router {
    crate::router(&crate::Config {
      listen: "127.0.0.1:0".parse().unwrap(),
      data_dir: directory.join("data"),
      web_dir: directory.join("web"),
      publish_token: "fixture-publishing-token-with-enough-characters".into(),
      oidc: None,
      oauth: Some(super::config()),
      account: Some(AccountConfig {
        issuer: self.issuer.clone(),
        client_id: CLIENT_ID.into(),
        client_secret: CLIENT_SECRET.into(),
        origin: super::config().origin,
      }),
      bootstrap_owner: None,
    })
    .unwrap()
  }

  async fn start_login(&self, app: &Router, cli_code: Option<&str>) -> Request<Body> {
    let path = cli_code
      .map(|code| format!("/api/v1/auth/account/login?cli_code={code}"))
      .unwrap_or_else(|| "/api/v1/auth/account/login".into());
    let login = get_page(app, &path).await;
    assert_eq!(login.status(), StatusCode::TEMPORARY_REDIRECT);
    let authorize = Url::parse(login.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(authorize.origin().ascii_serialization(), self.origin);
    assert_eq!(authorize.path(), "/oidc/auth");
    let fields: HashMap<_, _> = authorize.query_pairs().into_owned().collect();
    assert_eq!(fields["client_id"], CLIENT_ID);
    assert_eq!(fields["redirect_uri"], CALLBACK);
    assert_eq!(fields["response_type"], "code");
    assert_eq!(fields["scope"], "openid identities");
    assert_eq!(fields["code_challenge_method"], "S256");
    assert_ne!(fields["state"], fields["nonce"]);
    *self.authorization.lock().unwrap() = Some(Authorization {
      challenge: fields["code_challenge"].clone(),
      nonce: fields["nonce"].clone(),
    });
    let cookie = login.headers()[header::SET_COOKIE]
      .to_str()
      .unwrap()
      .split(';')
      .next()
      .unwrap();
    Request::get(format!(
      "/api/v1/auth/account/callback?state={}&code=fixture-code",
      fields["state"]
    ))
    .header(header::COOKIE, cookie)
    .body(Body::empty())
    .unwrap()
  }
}

async fn get_page(app: &Router, path: &str) -> Response {
  app
    .clone()
    .oneshot(Request::get(path).body(Body::empty()).unwrap())
    .await
    .unwrap()
}

fn session_cookie(response: &Response) -> Option<&str> {
  response
    .headers()
    .get_all(header::SET_COOKIE)
    .iter()
    .map(|value| value.to_str().unwrap().split(';').next().unwrap())
    .find_map(|cookie| cookie.strip_prefix(SESSION_PREFIX))
}

#[tokio::test]
async fn account_sign_in_preserves_existing_store_owner_and_disables_direct_github_login() {
  let (directory, store, github_app, old_secret) = fixture();
  store.register_package("sample", &super::user()).unwrap();
  let legacy_me = json_body(get_page(&github_app, "/api/v1/auth/me").await).await;
  assert_eq!(legacy_me["login_provider"], "github");
  for endpoint in ["login", "callback", "security"] {
    assert_eq!(
      get_page(&github_app, &format!("/api/v1/auth/account/{endpoint}"))
        .await
        .status(),
      StatusCode::SERVICE_UNAVAILABLE
    );
  }

  let issuer = MockAccount::new(json!({
    "github": {"userId": "42", "details": {"rawData": {"userInfo": {
      "id": 42, "login": "renamed-owner"
    }}}},
  }))
  .await;
  let app = issuer.router(directory.path());
  let me = app
    .clone()
    .oneshot(request(
      "GET",
      "/api/v1/auth/me",
      &old_secret,
      Body::empty(),
    ))
    .await
    .unwrap();
  let session = json_body(me).await;
  assert_eq!(session["login_provider"], "account");
  assert_eq!(session["user"]["github_id"], 42);
  for endpoint in ["login", "callback"] {
    assert_eq!(
      get_page(&app, &format!("/api/v1/auth/github/{endpoint}"))
        .await
        .status(),
      StatusCode::SERVICE_UNAVAILABLE
    );
  }
  let security = get_page(&app, "/api/v1/auth/account/security").await;
  assert_eq!(security.status(), StatusCode::SEE_OTHER);
  assert_eq!(
    security.headers()[header::LOCATION],
    format!("{}/account/security", issuer.origin)
  );

  let callback = issuer.start_login(&app, None).await;
  let replay_path = callback.uri().clone();
  let replay_cookie = callback.headers()[header::COOKIE].clone();
  let signed_in = app.clone().oneshot(callback).await.unwrap();
  assert_eq!(signed_in.status(), StatusCode::SEE_OTHER);
  assert_eq!(signed_in.headers()[header::LOCATION], "/account");
  assert_eq!(signed_in.headers()[header::CACHE_CONTROL], "no-store");
  let secret = session_cookie(&signed_in).unwrap();
  let package = app
    .clone()
    .oneshot(request(
      "GET",
      "/api/v1/account/packages/sample",
      secret,
      Body::empty(),
    ))
    .await
    .unwrap();
  assert_eq!(package.status(), StatusCode::OK);
  let package = json_body(package).await;
  assert_eq!(package["role"], "owner");
  assert_eq!(package["owner"]["github_id"], 42);
  assert_eq!(package["owner"]["github_login"], "renamed-owner");
  let replay = app
    .oneshot(
      Request::get(replay_path)
        .header(header::COOKIE, replay_cookie)
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(
    replay.headers()[header::LOCATION],
    "/account?auth_error=failed"
  );
  assert!(session_cookie(&replay).is_none());
}

#[tokio::test]
async fn migrated_account_signs_in_without_a_current_github_link() {
  let (directory, store, _github_app, _old_secret) = fixture();
  let owner = super::user();
  store.register_package("sample", &owner).unwrap();
  let flow = store.start_cli_authorization(now()).unwrap();
  for identities in [json!({}), json!({"github": {"userId": "99"}})] {
    let issuer = MockAccount::new(identities).await;
    store.bind_account(&issuer.issuer, SUBJECT, &owner).unwrap();
    let app = issuer.router(directory.path());
    let callback = issuer.start_login(&app, Some(&flow.user_code)).await;
    let signed_in = app.clone().oneshot(callback).await.unwrap();
    assert_eq!(
      signed_in.headers()[header::LOCATION],
      format!("/account/cli?code={}", flow.user_code)
    );
    let secret = session_cookie(&signed_in).unwrap();
    let package = app
      .oneshot(request(
        "GET",
        "/api/v1/account/packages/sample",
        secret,
        Body::empty(),
      ))
      .await
      .unwrap();
    assert_eq!(package.status(), StatusCode::OK);
    let package = json_body(package).await;
    assert_eq!(package["role"], "owner");
    assert_eq!(package["owner"]["github_id"], owner.github_id);
    assert_eq!(issuer.userinfo_requests.load(Ordering::Relaxed), 0);
  }
  assert!(store.user_by_id(99).unwrap().is_none());
}

#[tokio::test]
async fn account_sign_in_without_github_retains_pending_cli_request() {
  let (directory, store, _github_app, _old_secret) = fixture();
  let flow = store.start_cli_authorization(now()).unwrap();
  let issuer = MockAccount::new(json!({})).await;
  let app = issuer.router(directory.path());
  let callback = issuer.start_login(&app, Some(&flow.user_code)).await;
  let signed_out = app.oneshot(callback).await.unwrap();
  assert_eq!(signed_out.status(), StatusCode::SEE_OTHER);
  assert_eq!(
    signed_out.headers()[header::LOCATION],
    format!(
      "/account/cli?code={}&auth_error=github_link_required",
      flow.user_code
    )
  );
  assert!(session_cookie(&signed_out).is_none());
  assert_eq!(
    store
      .cli_authorization(&flow.user_code, now())
      .unwrap()
      .status,
    "pending"
  );
}

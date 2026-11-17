use super::*;
use axum::{
  http::HeaderMap,
  routing::{get, post},
  Form, Json, Router,
};
use serde_json::json;
use std::{
  collections::HashMap,
  sync::{Arc, Mutex},
};

mod http;

pub(super) fn config() -> OAuthConfig {
  OAuthConfig {
    client_id: "fixture-client".into(),
    client_secret: "fixture-secret".into(),
    origin: "https://store.example.invalid".into(),
  }
}

fn store(directory: &tempfile::TempDir) -> Store {
  let store = Store::open(directory.path()).unwrap();
  migrate(&store.connection().unwrap()).unwrap();
  store
}

fn user() -> User {
  User {
    github_id: 42,
    github_login: "example-user".into(),
  }
}

fn session_headers(secret: &str) -> HeaderMap {
  let mut headers = HeaderMap::new();
  headers.insert(
    "cookie",
    format!("{}={secret}", sessions::SESSION_COOKIE)
      .parse()
      .unwrap(),
  );
  headers.insert("origin", "https://store.example.invalid".parse().unwrap());
  headers.insert("x-csrf-token", csrf_token(secret).parse().unwrap());
  headers
}

#[test]
fn sessions_are_hashed_expire_rotate_and_require_same_origin_csrf() {
  let directory = tempfile::tempdir().unwrap();
  let store = store(&directory);
  store.upsert_user(&user()).unwrap();
  let secret = store.create_session(&user(), None, now()).unwrap();
  let saved: Vec<u8> = store
    .connection()
    .unwrap()
    .query_row("SELECT token_hash FROM auth_sessions", [], |row| row.get(0))
    .unwrap();
  assert_eq!(saved, hash(&secret));
  assert_ne!(saved, secret.as_bytes());
  let auth = Auth::new(config(), store.clone()).unwrap();
  let headers = session_headers(&secret);
  assert_eq!(auth.require_mutation(&headers).unwrap(), user());
  for invalid in [None, Some("https://other.example.invalid"), Some("null")] {
    let mut invalid_headers = headers.clone();
    invalid_headers.remove("origin");
    if let Some(origin) = invalid {
      invalid_headers.insert("origin", origin.parse().unwrap());
    }
    assert!(auth.require_mutation(&invalid_headers).is_err());
  }
  let mut missing_csrf = headers.clone();
  missing_csrf.remove("x-csrf-token");
  assert!(auth.require_mutation(&missing_csrf).is_err());
  let mut wrong_csrf = headers.clone();
  wrong_csrf.insert(
    "x-csrf-token",
    csrf_token("different-session").parse().unwrap(),
  );
  assert!(auth.require_mutation(&wrong_csrf).is_err());
  let replacement = store.create_session(&user(), Some(&secret), now()).unwrap();
  assert!(auth.user(&headers).is_err());
  assert_eq!(auth.user(&session_headers(&replacement)).unwrap(), user());
  assert!(store
    .session_user(&replacement, now() + sessions::SESSION_SECONDS)
    .is_err());
  store.delete_session(&replacement).unwrap();
  assert!(auth.user(&session_headers(&replacement)).is_err());
}

#[test]
fn login_names_can_change_without_changing_identity_and_cookie_duplicates_fail() {
  let directory = tempfile::tempdir().unwrap();
  let store = store(&directory);
  store.upsert_user(&user()).unwrap();
  let secret = store.create_session(&user(), None, now()).unwrap();
  let mut renamed = user();
  renamed.github_login = "renamed-user".into();
  store.upsert_user(&renamed).unwrap();
  assert_eq!(store.session_user(&secret, now()).unwrap(), renamed);
  let mut headers = session_headers(&secret);
  headers.append(
    "cookie",
    format!("{}={secret}", sessions::SESSION_COOKIE)
      .parse()
      .unwrap(),
  );
  assert!(sessions::cookie(&headers, sessions::SESSION_COOKIE).is_err());
  assert!(store.user_by_id(u64::MAX).unwrap().is_none());
}

#[test]
fn oauth_flow_is_bound_to_browser_single_use_expiring_and_bounded() {
  let directory = tempfile::tempdir().unwrap();
  let store = store(&directory);
  store
    .create_flow("state", "browser", "verifier", 1000)
    .unwrap();
  assert!(store
    .consume_flow("state", "another-browser", 1001)
    .is_err());
  assert_eq!(
    store.consume_flow("state", "browser", 1001).unwrap(),
    "verifier"
  );
  assert!(store.consume_flow("state", "browser", 1001).is_err());
  store
    .create_flow("state", "browser", "verifier", 1000)
    .unwrap();
  assert!(store.consume_flow("state", "browser", 1600).is_err());
  for index in 0..256 {
    store
      .create_flow(&format!("state-{index}"), "browser", "verifier", 2000)
      .unwrap();
  }
  assert_eq!(
    store
      .create_flow("overflow", "browser", "verifier", 2000)
      .unwrap_err()
      .status,
    StatusCode::TOO_MANY_REQUESTS
  );
  store
    .create_flow("after-expiry", "browser", "verifier", 2600)
    .unwrap();
}

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
  fn drop(&mut self) {
    self.0.abort();
  }
}

#[tokio::test]
async fn github_exchange_sends_verifier_fetches_identity_and_redacts_errors() {
  let captured = Arc::new(Mutex::new(HashMap::<String, String>::new()));
  let token_form = captured.clone();
  let app = Router::new()
    .route(
      "/token",
      post(move |Form(form): Form<HashMap<String, String>>| {
        *token_form.lock().unwrap() = form;
        async { Json(json!({"access_token":"fixture-access-token","token_type":"bearer"})) }
      }),
    )
    .route(
      "/user",
      get(|headers: HeaderMap| async move {
        assert_eq!(headers["authorization"], "Bearer fixture-access-token");
        Json(json!({"id":42,"login":"example-user","type":"User"}))
      }),
    )
    .route(
      "/users/example-user",
      get(|| async { Json(json!({"id":42,"login":"example-user","type":"User"})) }),
    )
    .route(
      "/repos/example/project",
      get(|| async { Json(json!({"id":25,"owner":{"id":15},"full_name":"example/project"})) }),
    )
    .route(
      "/bad-token",
      post(|| async { Json(json!({"error":"fixture-sensitive-provider-error"})) }),
    )
    .route(
      "/redirect",
      post(|| async { axum::response::Redirect::temporary("/token") }),
    );
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let origin = format!("http://{}/", listener.local_addr().unwrap());
  let _server = Server(tokio::spawn(async {
    axum::serve(listener, app).await.unwrap();
  }));
  let client = github::GitHub::fixture(
    origin.parse().unwrap(),
    format!("{origin}token").parse().unwrap(),
  );
  assert_eq!(
    client
      .sign_in(&config(), "fixture-code", "fixture-verifier")
      .await
      .unwrap(),
    user()
  );
  let form = captured.lock().unwrap().clone();
  assert_eq!(form["code_verifier"], "fixture-verifier");
  assert_eq!(
    form["redirect_uri"],
    "https://store.example.invalid/api/v1/auth/github/callback"
  );
  assert_eq!(form["client_secret"], "fixture-secret");
  assert_eq!(client.lookup_user("example-user").await.unwrap(), user());
  assert_eq!(
    client
      .lookup_repository("example/project")
      .await
      .unwrap()
      .repository_id,
    25
  );
  for invalid in ["../user", "example//project", "https://example/project"] {
    assert!(client.lookup_repository(invalid).await.is_err());
  }
  for path in ["bad-token", "redirect"] {
    let client = github::GitHub::fixture(
      origin.parse().unwrap(),
      format!("{origin}{path}").parse().unwrap(),
    );
    let error = client
      .sign_in(&config(), "fixture-code", "fixture-verifier")
      .await
      .unwrap_err();
    assert_eq!(error.code, "github_unavailable");
    assert!(!error.message.contains("fixture"));
  }
}

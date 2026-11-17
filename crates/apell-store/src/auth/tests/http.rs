use super::{config, session_headers, user};
use crate::{
  auth::{now, User},
  Config, Store,
};
use axum::{
  body::Body,
  http::{Request, StatusCode},
  response::Response,
  Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::io::{Cursor, Write};
use tower::ServiceExt;
use zip::{write::SimpleFileOptions, ZipWriter};

mod cli;

fn fixture() -> (tempfile::TempDir, Store, Router, String) {
  let directory = tempfile::tempdir().unwrap();
  let configuration = Config {
    listen: "127.0.0.1:0".parse().unwrap(),
    data_dir: directory.path().join("data"),
    web_dir: directory.path().join("web"),
    publish_token: "fixture-publishing-token-with-enough-characters".into(),
    oidc: None,
    oauth: Some(config()),
    bootstrap_owner: None,
  };
  let router = crate::router(&configuration).unwrap();
  let store = Store::open(&configuration.data_dir).unwrap();
  store.upsert_user(&user()).unwrap();
  let session = store.create_session(&user(), None, now()).unwrap();
  (directory, store, router, session)
}

fn request(method: &str, path: &str, secret: &str, body: impl Into<Body>) -> Request<Body> {
  let mut request = Request::builder()
    .method(method)
    .uri(path)
    .header("content-type", "application/json")
    .body(body.into())
    .unwrap();
  request.headers_mut().extend(session_headers(secret));
  request
}

async fn json_body(response: Response) -> Value {
  serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn unreadable_body() -> Body {
  Body::from_stream(futures_util::stream::poll_fn(
    |_| -> std::task::Poll<Option<std::result::Result<Vec<u8>, std::io::Error>>> {
      panic!("unauthenticated or CSRF-rejected bodies must not be polled")
    },
  ))
}

#[tokio::test]
async fn http_sign_in_state_csrf_and_package_registration() {
  let (_directory, store, app, secret) = fixture();
  let response = app
    .clone()
    .oneshot(Request::get("/api/v1/auth/me").body(Body::empty()).unwrap())
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  assert_eq!(response.headers()["cache-control"], "no-store");
  let body = json_body(response).await;
  assert_eq!(body["login_available"], true);
  assert!(body["user"].is_null());
  assert!(body["csrf_token"].is_null());
  let response = app
    .clone()
    .oneshot(request("GET", "/api/v1/auth/me", &secret, Body::empty()))
    .await
    .unwrap();
  let body = json_body(response).await;
  assert_eq!(body["user"]["github_id"], 42);
  assert_eq!(body["csrf_token"], super::super::csrf_token(&secret));

  let anonymous = Request::post("/api/v1/account/packages")
    .header("content-type", "application/json")
    .body(unreadable_body())
    .unwrap();
  assert_eq!(
    app.clone().oneshot(anonymous).await.unwrap().status(),
    StatusCode::UNAUTHORIZED
  );
  let mut csrf = request(
    "POST",
    "/api/v1/account/packages",
    &secret,
    unreadable_body(),
  );
  csrf.headers_mut().remove("x-csrf-token");
  assert_eq!(
    app.clone().oneshot(csrf).await.unwrap().status(),
    StatusCode::FORBIDDEN
  );

  let registration = request(
    "POST",
    "/api/v1/account/packages",
    &secret,
    r#"{"applet_id":"sample"}"#,
  );
  let response = app.clone().oneshot(registration).await.unwrap();
  assert_eq!(response.status(), StatusCode::CREATED);
  assert_eq!(response.headers()["cache-control"], "no-store");
  let details = json_body(response).await;
  assert_eq!(details["owner"]["github_id"], 42);
  assert_eq!(details["role"], "owner");
  let list = json_body(
    app
      .clone()
      .oneshot(request(
        "GET",
        "/api/v1/account/packages",
        &secret,
        Body::empty(),
      ))
      .await
      .unwrap(),
  )
  .await;
  assert_eq!(list["packages"][0]["applet_id"], "sample");
  assert_eq!(list["packages"][0]["published"], false);
  assert!(store.catalog().unwrap().applets.is_empty());
  let response = app
    .clone()
    .oneshot(request(
      "GET",
      "/api/v1/account/packages/sample",
      &secret,
      Body::empty(),
    ))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  let response = app
    .clone()
    .oneshot(request(
      "POST",
      "/api/v1/account/packages",
      &secret,
      r#"{"applet_id":"sample"}"#,
    ))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::CONFLICT);
  let response = app
    .clone()
    .oneshot(request("POST", "/api/v1/auth/logout", &secret, "{}"))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::NO_CONTENT);
  assert!(response.headers()["set-cookie"]
    .to_str()
    .unwrap()
    .contains("Max-Age=0"));
  assert_eq!(
    app
      .oneshot(request(
        "GET",
        "/api/v1/account/packages",
        &secret,
        Body::empty()
      ))
      .await
      .unwrap()
      .status(),
    StatusCode::UNAUTHORIZED
  );
}

#[tokio::test]
async fn http_invitation_acceptance_protects_owner_and_rechecks_membership() {
  let (_directory, store, app, secret) = fixture();
  store.register_package("sample", &user()).unwrap();
  let maintainer = User {
    github_id: 43,
    github_login: "example-maintainer".into(),
  };
  store.upsert_user(&maintainer).unwrap();
  let other_secret = store.create_session(&maintainer, None, now()).unwrap();
  let invitation = store.invite_maintainer("sample", 42, &maintainer).unwrap();
  let path = format!("/api/v1/account/invitations/{}/accept", invitation.id);
  assert_eq!(
    app
      .clone()
      .oneshot(request("POST", &path, &secret, "{}"))
      .await
      .unwrap()
      .status(),
    StatusCode::FORBIDDEN
  );
  let response = app
    .clone()
    .oneshot(request("POST", &path, &other_secret, "{}"))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  let response = app
    .clone()
    .oneshot(request(
      "GET",
      "/api/v1/account/packages/sample",
      &other_secret,
      Body::empty(),
    ))
    .await
    .unwrap();
  assert_eq!(json_body(response).await["role"], "maintainer");
  assert_eq!(
    app
      .clone()
      .oneshot(request(
        "DELETE",
        "/api/v1/account/packages/sample/members/42",
        &other_secret,
        "{}"
      ))
      .await
      .unwrap()
      .status(),
    StatusCode::CONFLICT
  );
  assert_eq!(
    app
      .clone()
      .oneshot(request(
        "DELETE",
        "/api/v1/account/packages/sample/members/43",
        &secret,
        "{}"
      ))
      .await
      .unwrap()
      .status(),
    StatusCode::OK
  );
  assert_eq!(
    app
      .oneshot(request(
        "GET",
        "/api/v1/account/packages/sample",
        &other_secret,
        Body::empty()
      ))
      .await
      .unwrap()
      .status(),
    StatusCode::FORBIDDEN
  );
}

fn archive(id: &str) -> Vec<u8> {
  let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
  let manifest = serde_json::to_vec(&json!({"schema_version":1,"api_version":2,"applet_id":id,"name":"Sample","version":"1.0.0","permissions":[],"network":false})).unwrap();
  for (path, bytes) in [
    ("manifest.json", manifest.as_slice()),
    ("index.html", b"<html>Sample</html>".as_slice()),
  ] {
    zip.start_file(path, SimpleFileOptions::default()).unwrap();
    zip.write_all(bytes).unwrap();
  }
  zip.finish().unwrap().into_inner()
}

fn upload_bytes(id: &str) -> Vec<u8> {
  let mut body = b"--fixture\r\nContent-Disposition: form-data; name=\"metadata\"\r\nContent-Type: application/json\r\n\r\n{\"description\":\"Sample package\",\"release_notes\":\"First release\"}\r\n--fixture\r\nContent-Disposition: form-data; name=\"package\"; filename=\"sample.apel\"\r\nContent-Type: application/octet-stream\r\n\r\n".to_vec();
  body.extend(archive(id));
  body.extend(b"\r\n--fixture--\r\n");
  body
}

fn upload_request(secret: &str, body: Body) -> Request<Body> {
  let mut request = request(
    "POST",
    "/api/v1/account/packages/sample/publish",
    secret,
    body,
  );
  request.headers_mut().insert(
    "content-type",
    "multipart/form-data; boundary=fixture".parse().unwrap(),
  );
  request
}

#[tokio::test]
async fn browser_upload_is_scoped_to_selected_package() {
  let (_directory, store, app, secret) = fixture();
  store.register_package("sample", &user()).unwrap();
  let response = app
    .clone()
    .oneshot(upload_request(&secret, Body::from(upload_bytes("other"))))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::BAD_REQUEST);
  assert!(store.catalog().unwrap().applets.is_empty());
  let response = app
    .oneshot(upload_request(&secret, Body::from(upload_bytes("sample"))))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::CREATED);
  assert_eq!(json_body(response).await["applet_id"], "sample");
  assert_eq!(store.detail("sample").unwrap().releases.len(), 1);
}

#[tokio::test]
async fn removing_a_maintainer_during_upload_prevents_publication() {
  let (directory, store, app, _secret) = fixture();
  store.register_package("sample", &user()).unwrap();
  let maintainer = User {
    github_id: 43,
    github_login: "example-maintainer".into(),
  };
  store.upsert_user(&maintainer).unwrap();
  let secret = store.create_session(&maintainer, None, now()).unwrap();
  let invitation = store.invite_maintainer("sample", 42, &maintainer).unwrap();
  store.accept_invitation(&invitation.id, 43).unwrap();
  let revoke = store.clone();
  let body = Body::from_stream(futures_util::stream::once(async move {
    revoke.remove_maintainer("sample", 42, 43).unwrap();
    Ok::<_, std::io::Error>(upload_bytes("sample"))
  }));
  let response = app.oneshot(upload_request(&secret, body)).await.unwrap();
  assert_eq!(response.status(), StatusCode::FORBIDDEN);
  assert!(store.catalog().unwrap().applets.is_empty());
  assert_eq!(
    std::fs::read_dir(directory.path().join("data/packages"))
      .unwrap()
      .count(),
    0
  );
}

#[tokio::test]
async fn oauth_http_callback_binds_pkce_creates_session_and_rejects_replay() {
  oauth_callback(false).await;
}

#[tokio::test]
async fn oauth_http_callback_resumes_cli_approval_without_approving_it() {
  oauth_callback(true).await;
}

async fn oauth_callback(resume_cli: bool) {
  use crate::auth::{github, Auth};
  use axum::{
    extract::Form,
    http::HeaderMap,
    routing::{get, post},
    Json,
  };
  use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
  use sha2::{Digest, Sha256};
  use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
  };
  use tokio::sync::Semaphore;

  let challenge = Arc::new(Mutex::new(String::new()));
  let expected = challenge.clone();
  let github = Router::new()
    .route(
      "/token",
      post(move |Form(form): Form<HashMap<String, String>>| {
        assert_eq!(
          URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes())),
          *expected.lock().unwrap()
        );
        async { Json(json!({"access_token":"fixture-access-token","token_type":"bearer"})) }
      }),
    )
    .route(
      "/user",
      get(|headers: HeaderMap| async move {
        assert_eq!(headers["authorization"], "Bearer fixture-access-token");
        Json(json!({"id":42,"login":"example-user","type":"User"}))
      }),
    );
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let base = format!("http://{}/", listener.local_addr().unwrap());
  let _server = super::Server(tokio::spawn(async {
    axum::serve(listener, github).await.unwrap();
  }));
  let directory = tempfile::tempdir().unwrap();
  let store = Store::open(directory.path()).unwrap();
  let cli = resume_cli.then(|| store.start_cli_authorization(now()).unwrap());
  let mut auth = Auth::new(config(), store.clone()).unwrap();
  auth.github = github::GitHub::fixture(
    base.parse().unwrap(),
    format!("{base}token").parse().unwrap(),
  );
  let state = crate::AppState {
    store: store.clone(),
    token_hash: [0; 32],
    oidc: None,
    auth: Some(Arc::new(auth)),
    uploads: Arc::new(Semaphore::new(1)),
    queries: Arc::new(Semaphore::new(4)),
    downloads: Arc::new(Semaphore::new(8)),
  };
  let app = Router::new()
    .nest("/api", crate::auth::routes())
    .layer(axum::middleware::from_fn(crate::response_headers))
    .with_state(state);
  let login = app
    .clone()
    .oneshot(
      Request::get(
        cli
          .as_ref()
          .map(|flow| format!("/api/v1/auth/github/login?cli_code={}", flow.user_code))
          .unwrap_or_else(|| "/api/v1/auth/github/login".into()),
      )
      .body(Body::empty())
      .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(login.status(), StatusCode::TEMPORARY_REDIRECT);
  let location: url::Url = login.headers()["location"]
    .to_str()
    .unwrap()
    .parse()
    .unwrap();
  let fields: HashMap<_, _> = location.query_pairs().into_owned().collect();
  *challenge.lock().unwrap() = fields["code_challenge"].clone();
  let browser_cookie = login.headers()["set-cookie"]
    .to_str()
    .unwrap()
    .split(';')
    .next()
    .unwrap()
    .to_owned();
  let callback = format!(
    "/api/v1/auth/github/callback?state={}&code=fixture-code",
    fields["state"]
  );
  let response = app
    .clone()
    .oneshot(
      Request::get(&callback)
        .header("cookie", &browser_cookie)
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::SEE_OTHER);
  assert_eq!(
    response.headers()["location"],
    cli
      .as_ref()
      .map(|flow| format!("/account/cli?code={}", flow.user_code))
      .unwrap_or_else(|| "/account".into())
  );
  if let Some(flow) = &cli {
    assert_eq!(
      store
        .cli_authorization(&flow.user_code, now())
        .unwrap()
        .status,
      "pending"
    );
  }
  assert_eq!(response.headers()["cache-control"], "no-store");
  let session_cookie = response
    .headers()
    .get_all("set-cookie")
    .iter()
    .find_map(|cookie| {
      let cookie = cookie.to_str().unwrap();
      cookie
        .starts_with("__Host-apell_session=")
        .then(|| cookie.split(';').next().unwrap().to_owned())
    })
    .unwrap();
  let me = app
    .clone()
    .oneshot(
      Request::get("/api/v1/auth/me")
        .header("cookie", session_cookie)
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  let body = json_body(me).await;
  assert_eq!(body["user"]["github_id"], 42);
  assert!(body["csrf_token"].as_str().is_some());
  assert!(!body.to_string().contains("fixture-access-token"));
  let replay = app
    .oneshot(
      Request::get(callback)
        .header("cookie", browser_cookie)
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(replay.headers()["location"], "/account?auth_error=failed");
  assert!(replay
    .headers()
    .get_all("set-cookie")
    .iter()
    .all(|cookie| !cookie
      .to_str()
      .unwrap()
      .starts_with("__Host-apell_session=")));
}

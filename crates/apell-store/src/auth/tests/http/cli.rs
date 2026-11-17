use super::*;
use crate::auth::cli::Polled;

fn bearer(method: &str, path: &str, token: &str, body: impl Into<Body>) -> Request<Body> {
  Request::builder()
    .method(method)
    .uri(path)
    .header("content-type", "application/json")
    .header("authorization", format!("Bearer {token}"))
    .body(body.into())
    .unwrap()
}

fn json_request(path: &str, body: impl Into<Body>) -> Request<Body> {
  Request::post(path)
    .header("content-type", "application/json")
    .body(body.into())
    .unwrap()
}

fn credential(store: &Store, user: &User) -> String {
  let flow = store.start_cli_authorization(now()).unwrap();
  store
    .decide_cli_authorization(&flow.user_code, user.github_id, true, now())
    .unwrap();
  let Polled::Authorized(credential) = store
    .poll_cli_authorization(&flow.device_code, now())
    .unwrap()
  else {
    panic!("approval must issue a credential")
  };
  credential.token
}

#[tokio::test]
async fn approval_requires_browser_csrf_and_issues_one_revocable_account_credential() {
  let (_directory, store, app, session) = fixture();
  let started = app
    .clone()
    .oneshot(json_request("/api/v1/auth/cli/start", "{}"))
    .await
    .unwrap();
  assert_eq!(started.status(), StatusCode::OK);
  assert_eq!(started.headers()["cache-control"], "no-store");
  let started = json_body(started).await;
  let device = started["device_code"].as_str().unwrap();
  let code = started["user_code"].as_str().unwrap();
  assert_eq!(started["interval"], 3);
  assert_eq!(
    started["verification_uri"],
    "https://store.example.invalid/account/cli"
  );
  assert_eq!(
    started["verification_uri_complete"],
    format!("https://store.example.invalid/account/cli?code={code}")
  );
  assert!(!started["verification_uri_complete"]
    .as_str()
    .unwrap()
    .contains(device));
  let path = format!("/api/v1/account/cli/requests/{code}");
  let details = app
    .clone()
    .oneshot(request("GET", &path, &session, Body::empty()))
    .await
    .unwrap();
  assert_eq!(json_body(details).await["status"], "pending");
  assert_eq!(
    app
      .clone()
      .oneshot(Request::get(&path).body(Body::empty()).unwrap())
      .await
      .unwrap()
      .status(),
    StatusCode::UNAUTHORIZED
  );
  let approval = format!("{path}/approve");
  let mut no_csrf = request("POST", &approval, &session, unreadable_body());
  no_csrf.headers_mut().remove("x-csrf-token");
  assert_eq!(
    app.clone().oneshot(no_csrf).await.unwrap().status(),
    StatusCode::FORBIDDEN
  );
  let response = app
    .clone()
    .oneshot(request("POST", &approval, &session, "{}"))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  let poll = json!({"device_code":device}).to_string();
  let response = app
    .clone()
    .oneshot(json_request("/api/v1/auth/cli/poll", poll.clone()))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  let approved = json_body(response).await;
  let token = approved["token"].as_str().unwrap();
  assert!(token.starts_with("apel_user_"));
  assert_eq!(approved["user"]["github_id"], 42);
  assert_eq!(
    app
      .clone()
      .oneshot(json_request("/api/v1/auth/cli/poll", poll))
      .await
      .unwrap()
      .status(),
    StatusCode::BAD_REQUEST
  );
  let me = app
    .clone()
    .oneshot(bearer("GET", "/api/v1/auth/cli/me", token, Body::empty()))
    .await
    .unwrap();
  assert_eq!(me.status(), StatusCode::OK);
  assert_eq!(json_body(me).await["expires_at"], approved["expires_at"]);
  let registered = app
    .clone()
    .oneshot(bearer(
      "POST",
      "/api/v1/account/packages",
      token,
      r#"{"applet_id":"sample"}"#,
    ))
    .await
    .unwrap();
  assert_eq!(registered.status(), StatusCode::CREATED);
  assert_eq!(
    store.package_details("sample", 42).unwrap().owner.github_id,
    42
  );
  let listed = app
    .clone()
    .oneshot(request(
      "GET",
      "/api/v1/account/cli/credentials",
      &session,
      Body::empty(),
    ))
    .await
    .unwrap();
  let listed = json_body(listed).await;
  assert_eq!(listed["credentials"].as_array().unwrap().len(), 1);
  assert!(!listed.to_string().contains(token));
  let id = listed["credentials"][0]["id"].as_str().unwrap();
  let revoke = format!("/api/v1/account/cli/credentials/{id}");
  let response = app
    .clone()
    .oneshot(request("DELETE", &revoke, &session, "{}"))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  assert_eq!(
    app
      .clone()
      .oneshot(bearer("GET", "/api/v1/auth/cli/me", token, Body::empty()))
      .await
      .unwrap()
      .status(),
    StatusCode::UNAUTHORIZED
  );
  assert_eq!(
    app
      .oneshot(bearer(
        "GET",
        "/api/v1/account/packages",
        token,
        Body::empty()
      ))
      .await
      .unwrap()
      .status(),
    StatusCode::UNAUTHORIZED
  );
}

#[tokio::test]
async fn cli_http_pending_denied_expired_and_input_bounds() {
  let (_directory, store, app, session) = fixture();
  let flow = store.start_cli_authorization(now()).unwrap();
  let poll = json!({"device_code":flow.device_code}).to_string();
  let pending = app
    .clone()
    .oneshot(json_request("/api/v1/auth/cli/poll", poll.clone()))
    .await
    .unwrap();
  assert_eq!(pending.status(), StatusCode::ACCEPTED);
  assert_eq!(
    json_body(pending).await,
    json!({"status":"pending","interval":3})
  );
  assert_eq!(
    app
      .clone()
      .oneshot(json_request("/api/v1/auth/cli/poll", poll))
      .await
      .unwrap()
      .status(),
    StatusCode::TOO_MANY_REQUESTS
  );
  let denied = store.start_cli_authorization(now()).unwrap();
  let response = app
    .clone()
    .oneshot(request(
      "POST",
      &format!("/api/v1/account/cli/requests/{}/deny", denied.user_code),
      &session,
      "{}",
    ))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  let response = app
    .clone()
    .oneshot(json_request(
      "/api/v1/auth/cli/poll",
      json!({"device_code":denied.device_code}).to_string(),
    ))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::FORBIDDEN);
  assert_eq!(
    json_body(response).await["error"]["code"],
    "authorization_denied"
  );
  let expired = store.start_cli_authorization(now() - 600).unwrap();
  let response = app
    .clone()
    .oneshot(json_request(
      "/api/v1/auth/cli/poll",
      json!({"device_code":expired.device_code}).to_string(),
    ))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::BAD_REQUEST);
  for body in [
    r#"{"extra":true}"#.to_owned(),
    " ".repeat(4097),
    "null".into(),
  ] {
    assert_eq!(
      app
        .clone()
        .oneshot(json_request("/api/v1/auth/cli/start", body))
        .await
        .unwrap()
        .status(),
      StatusCode::BAD_REQUEST
    );
  }
  assert_eq!(
    app
      .clone()
      .oneshot(
        Request::post("/api/v1/auth/cli/start")
          .header("content-type", "text/plain")
          .body(Body::from("{}"))
          .unwrap()
      )
      .await
      .unwrap()
      .status(),
    StatusCode::BAD_REQUEST
  );
  assert!(store.cli_credentials(42, now()).unwrap().is_empty());
}

#[tokio::test]
async fn bearer_never_approves_or_uses_cookie_fallback_and_logout_is_independent() {
  let (_directory, store, app, session) = fixture();
  let token = credential(&store, &user());
  let flow = store.start_cli_authorization(now()).unwrap();
  let approval = format!("/api/v1/account/cli/requests/{}/approve", flow.user_code);
  for path in [&approval, "/api/v1/auth/logout"] {
    let mut req = bearer("POST", path, &token, unreadable_body());
    req.headers_mut().extend(session_headers(&session));
    assert_eq!(
      app.clone().oneshot(req).await.unwrap().status(),
      StatusCode::UNAUTHORIZED
    );
  }
  let browser_me = app
    .clone()
    .oneshot(bearer("GET", "/api/v1/auth/me", &token, Body::empty()))
    .await
    .unwrap();
  assert!(json_body(browser_me).await["user"].is_null());
  assert_eq!(
    app
      .clone()
      .oneshot(bearer(
        "GET",
        "/api/v1/account/cli/credentials",
        &token,
        Body::empty()
      ))
      .await
      .unwrap()
      .status(),
    StatusCode::UNAUTHORIZED
  );
  let mut invalid = request(
    "POST",
    "/api/v1/account/packages",
    &session,
    unreadable_body(),
  );
  invalid
    .headers_mut()
    .insert("authorization", "Bearer apel_user_invalid".parse().unwrap());
  assert_eq!(
    app.clone().oneshot(invalid).await.unwrap().status(),
    StatusCode::UNAUTHORIZED
  );
  assert_eq!(
    app
      .clone()
      .oneshot(request(
        "POST",
        "/api/v1/auth/cli/logout",
        &session,
        unreadable_body()
      ))
      .await
      .unwrap()
      .status(),
    StatusCode::UNAUTHORIZED
  );
  assert_eq!(
    app
      .clone()
      .oneshot(bearer("POST", "/api/v1/auth/cli/logout", &token, "{}"))
      .await
      .unwrap()
      .status(),
    StatusCode::OK
  );
  assert_eq!(
    app
      .clone()
      .oneshot(bearer("GET", "/api/v1/auth/cli/me", &token, Body::empty()))
      .await
      .unwrap()
      .status(),
    StatusCode::UNAUTHORIZED
  );
  assert_eq!(
    json_body(
      app
        .oneshot(request("GET", "/api/v1/auth/me", &session, Body::empty()))
        .await
        .unwrap()
    )
    .await["user"]["github_id"],
    42
  );
}

fn upload(token: &str, id: &str) -> Request<Body> {
  let mut request = bearer("POST", "/api/v1/publish", token, upload_bytes(id));
  request.headers_mut().insert(
    "content-type",
    "multipart/form-data; boundary=fixture".parse().unwrap(),
  );
  request
}

#[tokio::test]
async fn cli_global_publish_uses_current_membership_and_cannot_fall_back_to_browser() {
  let (_directory, store, app, session) = fixture();
  store.register_package("sample", &user()).unwrap();
  let owner = credential(&store, &user());
  let maintainer = User {
    github_id: 43,
    github_login: "example-maintainer".into(),
  };
  store.upsert_user(&maintainer).unwrap();
  let token = credential(&store, &maintainer);
  assert_eq!(
    app
      .clone()
      .oneshot(upload(&token, "sample"))
      .await
      .unwrap()
      .status(),
    StatusCode::FORBIDDEN
  );
  let invitation = store.invite_maintainer("sample", 42, &maintainer).unwrap();
  store.accept_invitation(&invitation.id, 43).unwrap();
  store.remove_maintainer("sample", 42, 43).unwrap();
  assert_eq!(
    app
      .clone()
      .oneshot(upload(&token, "sample"))
      .await
      .unwrap()
      .status(),
    StatusCode::FORBIDDEN
  );
  let mut invalid = bearer(
    "POST",
    "/api/v1/publish",
    "apel_user_invalid",
    unreadable_body(),
  );
  invalid.headers_mut().extend(session_headers(&session));
  invalid.headers_mut().insert(
    "content-type",
    "multipart/form-data; boundary=fixture".parse().unwrap(),
  );
  assert_eq!(
    app.clone().oneshot(invalid).await.unwrap().status(),
    StatusCode::UNAUTHORIZED
  );
  let response = app.oneshot(upload(&owner, "sample")).await.unwrap();
  assert_eq!(response.status(), StatusCode::CREATED);
  assert_eq!(store.detail("sample").unwrap().releases.len(), 1);
}

mod fixture;

use super::*;
use axum::{
  body::Body,
  http::{HeaderValue, Request as HttpRequest},
};
use fixture::{claims, config, exchange_request, key, signed, JwksServer};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

#[test]
fn diagnostic_categories_never_include_rejected_claim_values() {
  use diagnostics::{decode_reason, Rejection};
  use jsonwebtoken::errors::ErrorKind;
  let detail = "sensitive-rejected-claim-value";
  for error in [
    ErrorKind::MissingRequiredClaim(detail.into()),
    ErrorKind::InvalidClaimFormat(detail.into()),
    ErrorKind::Json(
      serde_json::from_str::<u64>(&format!("\"{detail}\""))
        .unwrap_err()
        .into(),
    ),
  ] {
    let reason = decode_reason(&error);
    assert!(matches!(
      reason,
      Rejection::TokenMissingClaim | Rejection::TokenClaimFormat
    ));
    assert!(!reason.code().contains(detail));
  }
  let mut identity: Claims = serde_json::from_value(claims("diagnostic")).unwrap();
  let policy = config().publishers;
  identity.repository_id = "999".into();
  assert_eq!(
    identity.policy_failure(&policy, None),
    Rejection::PolicyRepositoryId
  );
  identity.repository_id = "123".into();
  identity.workflow_ref = detail.into();
  assert_eq!(
    identity.policy_failure(&policy, None),
    Rejection::PolicyWorkflow
  );
  assert_eq!(
    identity.policy_failure(&policy, Some("other")),
    Rejection::PolicyApplet
  );
}

async fn service() -> (Oidc, JwksServer) {
  let server = JwksServer::new(key().jwks.clone()).await;
  let store = Store::open(server.root.path().join("data")).unwrap();
  store
    .bootstrap_packages(&config().publishers, None)
    .unwrap();
  let mut service = Oidc::new(config(), store).unwrap();
  service.keys = Arc::new(keys::Keys::fixture(&server.url));
  (service, server)
}

#[test]
fn policy_files_are_bounded_and_explicit_without_name_based_identity() {
  let root = tempfile::tempdir().unwrap();
  let path = root.path().join("policy.json");
  let valid = json!({"publishers":[{
    "applet_id":"sample", "repository_id":123, "repository_owner_id":456,
    "workflow":".github/workflows/publish.yml", "allowed_refs":["refs/heads/main"],
    "event_names":["workflow_dispatch"], "environment":"release"
  }]});
  std::fs::write(&path, serde_json::to_vec(&valid).unwrap()).unwrap();
  let loaded = OidcConfig::load("https://STORE.example:443/", &path).unwrap();
  assert_eq!(loaded.audience, "https://store.example");
  assert_eq!(loaded.publishers[0].repository_id, 123);
  for invalid in [
    "http://store.example",
    "https://@store.example",
    "https://store.example/?key=value",
    "https://store.example/..",
    "https://store.example/#fragment",
    "https://store.example\\path",
  ] {
    assert!(OidcConfig::load(invalid, &path).is_err());
  }
  let mutations: [fn(&mut Value); 8] = [
    |p| p["publishers"][0]["repository_id"] = json!(0),
    |p| p["publishers"][0]["repository_owner_id"] = json!("owner"),
    |p| p["publishers"][0]["workflow"] = json!(".github/workflows/../publish.yml"),
    |p| p["publishers"][0]["allowed_refs"] = json!(["refs/heads/*"]),
    |p| p["publishers"][0]["allowed_refs"] = json!([]),
    |p| p["publishers"][0]["event_names"] = json!(["pull_request_target"]),
    |p| p["publishers"][0]["environment"] = json!(""),
    |p| p["publishers"][0]["unknown_field"] = json!("example/project"),
  ];
  for mutate in mutations {
    let mut data = valid.clone();
    mutate(&mut data);
    std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    assert!(OidcConfig::load("https://store.example", &path).is_err());
  }
  std::fs::write(&path, vec![b' '; 65537]).unwrap();
  assert!(OidcConfig::load("https://store.example", &path).is_err());
  let mut duplicate = config();
  duplicate.publishers.push(duplicate.publishers[0].clone());
  assert!(duplicate.validate().is_err());
}

#[tokio::test]
async fn signed_identity_mints_one_scoped_credential_with_bounded_lifetime() {
  let (service, server) = service().await;
  let identity = claims("first");
  let jwt = signed(&identity);
  let response = service
    .exchange(exchange_request(&jwt, "sample", "a".repeat(64)))
    .await
    .unwrap();
  assert!(response.token.starts_with("apel_ci_"));
  assert_eq!(response.token.len(), 72);
  assert!(
    response.expires_at <= now() + 300 && response.expires_at <= identity["exp"].as_u64().unwrap()
  );
  let mut headers = HeaderMap::new();
  headers.insert(
    header::AUTHORIZATION,
    HeaderValue::from_str(&format!("Bearer {}", response.token)).unwrap(),
  );
  let grant = service.authenticate(&headers).unwrap();
  assert_eq!(grant.applet_id, "sample");
  assert_eq!(grant.sha256, "a".repeat(64));
  assert!(service
    .exchange(exchange_request(&jwt, "sample", "a".repeat(64)))
    .await
    .is_err());
  assert_eq!(server.requests(), 1);
}

#[tokio::test]
async fn direct_jobs_may_repeat_their_authorized_workflow_reference() {
  let (service, _server) = service().await;
  let mut identity = claims("direct-job");
  identity["job_workflow_ref"] = identity["workflow_ref"].clone();
  service
    .exchange(exchange_request(
      &signed(&identity),
      "sample",
      "a".repeat(64),
    ))
    .await
    .unwrap();

  identity["jti"] = json!("different-job");
  identity["job_workflow_ref"] =
    json!("example/shared/.github/workflows/publish.yml@refs/heads/main");
  assert!(service
    .exchange(exchange_request(
      &signed(&identity),
      "sample",
      "a".repeat(64)
    ))
    .await
    .is_err());

  // Matching each other never substitutes for matching the configured workflow.
  identity["jti"] = json!("unauthorized-self-reference");
  identity["workflow_ref"] = json!("example/project/.github/workflows/other.yml@refs/heads/main");
  identity["job_workflow_ref"] = identity["workflow_ref"].clone();
  assert!(service
    .exchange(exchange_request(
      &signed(&identity),
      "sample",
      "a".repeat(64)
    ))
    .await
    .is_err());
}

#[tokio::test]
async fn signed_claims_must_match_repository_workflow_ref_environment_and_audience() {
  let (service, _server) = service().await;
  let mutations: [fn(&mut Value); 18] = [
    |c| c["iss"] = json!("https://other.example"),
    |c| c["aud"] = json!("https://other.example"),
    |c| c["aud"] = json!(["https://store.example", "https://other.example"]),
    |c| c["repository_id"] = json!("999"),
    |c| c["repository_owner_id"] = json!("999"),
    |c| c["workflow_ref"] = json!("example/project/.github/workflows/other.yml@refs/heads/main"),
    |c| c["workflow_ref"] = json!("another/project/.github/workflows/publish.yml@refs/heads/main"),
    |c| c["ref"] = json!("refs/heads/feature"),
    |c| c["environment"] = json!("other"),
    |c| {
      c.as_object_mut()
        .unwrap()
        .remove("environment")
        .map(|_| ())
        .unwrap()
    },
    |c| c["event_name"] = json!("pull_request_target"),
    |c| c["runner_environment"] = json!("self-hosted"),
    |c| {
      c["job_workflow_ref"] = json!("example/shared/.github/workflows/publish.yml@refs/heads/main")
    },
    |c| c["exp"] = json!(now() - 1),
    |c| c["nbf"] = json!(now() + 120),
    |c| c["iat"] = json!(now() + 120),
    |c| c["exp"] = json!(now() + 3600),
    |c| {
      c.as_object_mut().unwrap().remove("jti");
    },
  ];
  for (index, mutate) in mutations.into_iter().enumerate() {
    let mut identity = claims(&format!("invalid-{index}"));
    mutate(&mut identity);
    let response = service
      .exchange(exchange_request(
        &signed(&identity),
        "sample",
        "a".repeat(64),
      ))
      .await;
    assert!(response.is_err(), "mutation {index}");
  }
  // Subject formats and repository names may change; signed immutable IDs remain authoritative.
  let mut renamed = claims("renamed");
  renamed["sub"] = json!("repo:renamed@456/project@123:custom:value");
  renamed["repository"] = json!("renamed/project");
  renamed["workflow_ref"] = json!("renamed/project/.github/workflows/publish.yml@refs/heads/main");
  service
    .exchange(exchange_request(
      &signed(&renamed),
      "sample",
      "a".repeat(64),
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn jwt_header_algorithm_signature_and_key_sources_are_not_trusted() {
  let (service, server) = service().await;
  let mut header = jsonwebtoken::Header::new(Algorithm::RS256);
  header.kid = Some("fixture-key".into());
  header.jku = Some("https://other.example/keys".into());
  let redirected = jsonwebtoken::encode(&header, &claims("jku"), &key().encoding).unwrap();
  assert!(service
    .exchange(exchange_request(&redirected, "sample", "a".repeat(64)))
    .await
    .is_err());
  assert_eq!(server.requests(), 0);
  let key = jsonwebtoken::EncodingKey::from_secret(b"untrusted").unwrap();
  let hmac = jsonwebtoken::encode(
    &jsonwebtoken::Header::new(Algorithm::HS256),
    &claims("hmac"),
    &key,
  )
  .unwrap();
  assert!(service
    .exchange(exchange_request(&hmac, "sample", "a".repeat(64)))
    .await
    .is_err());
  let mut tampered = signed(&claims("tampered")).into_bytes();
  let index = tampered.len() - 12;
  tampered[index] = if tampered[index] == b'A' { b'B' } else { b'A' };
  assert!(service
    .exchange(exchange_request(
      &String::from_utf8(tampered).unwrap(),
      "sample",
      "a".repeat(64)
    ))
    .await
    .is_err());
}

#[tokio::test]
async fn jwks_cache_is_singleflight_throttled_and_never_follows_redirects() {
  let server = JwksServer::new(key().jwks.clone()).await;
  let keys = Arc::new(keys::Keys::fixture(&server.url));
  let mut tasks = Vec::new();
  for _ in 0..8 {
    let keys = keys.clone();
    tasks.push(tokio::spawn(async move {
      keys.get("fixture-key").await.is_ok()
    }));
  }
  for task in tasks {
    assert!(task.await.unwrap());
  }
  assert_eq!(server.requests(), 1);
  for _ in 0..8 {
    assert!(keys.get("unknown").await.is_err());
  }
  assert_eq!(server.requests(), 1);
  server.replace(StatusCode::FOUND, json!({}));
  keys.expire_for_test().await;
  assert!(keys.get("fixture-key").await.is_err());
  assert_eq!(server.requests(), 2);
  assert!(keys.get("fixture-key").await.is_err());
  assert_eq!(server.requests(), 2);
}

#[tokio::test]
async fn jwks_refresh_accepts_rotated_keys_and_rejects_invalid_key_sets() {
  let server = JwksServer::new(key().jwks.clone()).await;
  let keys = keys::Keys::fixture(&server.url);
  keys.get("fixture-key").await.unwrap();
  let mut rotated = key().jwks.clone();
  rotated["keys"][0]["kid"] = json!("rotated-key");
  server.replace(StatusCode::OK, rotated);
  keys.expire_for_test().await;
  keys.get("rotated-key").await.unwrap();
  assert!(keys.get("fixture-key").await.is_err());
  assert_eq!(server.requests(), 2);
  let mutations: [fn(&mut Value); 5] = [
    |set| set["keys"][0]["alg"] = json!("HS256"),
    |set| set["keys"][0]["n"] = json!("AQAB"),
    |set| set["keys"][0]["e"] = json!("Aw"),
    |set| set["keys"] = json!([set["keys"][0].clone(), set["keys"][0].clone()]),
    |set| set["padding"] = json!("x".repeat(256 * 1024)),
  ];
  for mutate in mutations {
    let mut invalid = key().jwks.clone();
    mutate(&mut invalid);
    server.replace(StatusCode::OK, invalid);
    keys.expire_for_test().await;
    assert!(keys.get("fixture-key").await.is_err());
  }
}

#[tokio::test]
async fn valid_identity_cannot_exchange_for_another_applet_or_malformed_body() {
  let (service, _server) = service().await;
  let jwt = signed(&claims("scope"));
  assert!(service
    .exchange(exchange_request(&jwt, "other", "a".repeat(64)))
    .await
    .is_err());
  assert!(service
    .exchange(exchange_request(&jwt, "sample", "invalid".into()))
    .await
    .is_err());
  assert!(service
    .exchange(exchange_request(&jwt, "sample", "A".repeat(64)))
    .await
    .is_err());
  let response = service
    .exchange(exchange_request(&jwt, "sample", "a".repeat(64)))
    .await
    .unwrap();
  assert!(response.expires_at > now());
}

#[tokio::test]
async fn exchange_authentication_happens_before_the_request_body_is_polled() {
  let (service, _server) = service().await;
  let mut foreign = claims("foreign");
  foreign["repository_id"] = json!("999");
  for authorization in [None, Some(format!("Bearer {}", signed(&foreign)))] {
    let body = Body::from_stream(futures_util::stream::poll_fn(
      |_| -> std::task::Poll<Option<std::result::Result<Vec<u8>, std::io::Error>>> {
        panic!("Untrusted identity body polled")
      },
    ));
    let mut request = HttpRequest::post("/api/v1/auth/github-actions");
    if let Some(value) = authorization {
      request = request.header(header::AUTHORIZATION, value);
    }
    assert!(service.exchange(request.body(body).unwrap()).await.is_err());
  }
}

#[test]
fn expired_credentials_and_replay_ids_cannot_bypass_capacity_limits() {
  let mut tokens = Tokens::default();
  let stamp = now();
  let mut identity: Claims = serde_json::from_value(claims("expiry")).unwrap();
  identity.exp = stamp + 600;
  let minted = tokens
    .mint(&identity, "sample".into(), "a".repeat(64), 1, stamp)
    .unwrap();
  assert!(tokens
    .authenticate(&minted.token, minted.expires_at)
    .is_err());
  assert!(tokens
    .mint(
      &identity,
      "sample".into(),
      "a".repeat(64),
      1,
      minted.expires_at
    )
    .is_err());
  for index in 0..1024 {
    let mut identity: Claims =
      serde_json::from_value(claims(&format!("capacity-{index}"))).unwrap();
    identity.exp = stamp + 600;
    let result = tokens.mint(&identity, "sample".into(), "a".repeat(64), 1, stamp);
    if index == 1023 {
      assert!(result.is_err());
    } else {
      result.unwrap();
    }
  }
}

#[tokio::test]
async fn concurrent_exchanges_for_one_identity_mint_exactly_once() {
  let (service, _server) = service().await;
  let jwt = signed(&claims("concurrent"));
  let (left, right) = tokio::join!(
    service.exchange(exchange_request(&jwt, "sample", "a".repeat(64))),
    service.exchange(exchange_request(&jwt, "sample", "a".repeat(64))),
  );
  assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
}

#[tokio::test]
async fn scoped_uploads_check_actual_package_identity_and_keep_admin_publishing() {
  let (service, _server) = service().await;
  let root = tempfile::tempdir().unwrap();
  let config = crate::Config {
    listen: "127.0.0.1:0".parse().unwrap(),
    data_dir: _server.root.path().join("data"),
    web_dir: root.path().join("web"),
    publish_token: "fixture-administrator-token-12345678".into(),
    oidc: None,
    oauth: None,
    bootstrap_owner: None,
  };
  let app = crate::router_with_identity(&config, Some(service.clone())).unwrap();
  let package = fixture::package("sample", "1.0.0");
  let jwt = signed(&claims("upload"));
  let exchange = app
    .clone()
    .oneshot(exchange_request(
      &jwt,
      "sample",
      apell_package::digest(&package),
    ))
    .await
    .unwrap();
  assert_eq!(exchange.status(), StatusCode::OK);
  assert_eq!(exchange.headers()[header::CACHE_CONTROL], "no-store");
  let body: Value =
    serde_json::from_slice(&exchange.into_body().collect().await.unwrap().to_bytes()).unwrap();
  let credential = body["token"].as_str().unwrap();
  let wrong = fixture::package("other", "1.0.0");
  let denied = app
    .clone()
    .oneshot(fixture::upload(credential, &wrong))
    .await
    .unwrap();
  assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
  assert_eq!(
    std::fs::read_dir(config.data_dir.join("packages"))
      .unwrap()
      .count(),
    0
  );
  let published = app
    .clone()
    .oneshot(fixture::upload(credential, &package))
    .await
    .unwrap();
  assert_eq!(published.status(), StatusCode::CREATED);
  let retried = app
    .clone()
    .oneshot(fixture::upload(credential, &package))
    .await
    .unwrap();
  assert_eq!(retried.status(), StatusCode::OK);
  let admin = app
    .oneshot(fixture::upload(
      &config.publish_token,
      &fixture::package("another", "1.0.0"),
    ))
    .await
    .unwrap();
  assert_eq!(admin.status(), StatusCode::CREATED);
  // Even a correctly hashed archive cannot substitute the applet identity selected at exchange.
  let token = service
    .exchange(exchange_request(
      &signed(&claims("wrong-id")),
      "sample",
      apell_package::digest(&wrong),
    ))
    .await
    .unwrap();
  let mut headers = HeaderMap::new();
  headers.insert(
    header::AUTHORIZATION,
    HeaderValue::from_str(&format!("Bearer {}", token.token)).unwrap(),
  );
  assert!(service
    .authenticate(&headers)
    .unwrap()
    .validate_package(&wrong, now())
    .is_err());
}

#[tokio::test]
async fn credential_expiration_during_upload_prevents_publication() {
  let (service, _server) = service().await;
  let root = tempfile::tempdir().unwrap();
  let config = crate::Config {
    listen: "127.0.0.1:0".parse().unwrap(),
    data_dir: _server.root.path().join("data"),
    web_dir: root.path().join("web"),
    publish_token: "fixture-administrator-token-12345678".into(),
    oidc: None,
    oauth: None,
    bootstrap_owner: None,
  };
  let app = crate::router_with_identity(&config, Some(service.clone())).unwrap();
  let package = fixture::package("sample", "1.0.0");
  let mut identity = claims("slow-upload");
  identity["exp"] = json!(now() + 2);
  let credential = service
    .exchange(exchange_request(
      &signed(&identity),
      "sample",
      apell_package::digest(&package),
    ))
    .await
    .unwrap();
  let (parts, body) = fixture::upload(&credential.token, &package).into_parts();
  let bytes = body.collect().await.unwrap().to_bytes();
  let delayed = Body::from_stream(futures_util::stream::once(async move {
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    Ok::<_, std::io::Error>(bytes)
  }));
  let response = app
    .oneshot(HttpRequest::from_parts(parts, delayed))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
  assert_eq!(
    std::fs::read_dir(config.data_dir.join("packages"))
      .unwrap()
      .count(),
    0
  );
}

#[tokio::test]
async fn database_policy_revocation_rejects_issued_tokens_and_in_flight_uploads() {
  let (service, server) = service().await;
  let owner = crate::auth::User {
    github_id: 1,
    github_login: "example-owner".into(),
  };
  service.store.bootstrap_packages(&[], Some(&owner)).unwrap();
  let settings = crate::Config {
    listen: "127.0.0.1:0".parse().unwrap(),
    data_dir: server.root.path().join("data"),
    web_dir: server.root.path().join("web"),
    publish_token: "fixture-administrator-token-12345678".into(),
    oidc: None,
    oauth: None,
    bootstrap_owner: None,
  };
  let app = crate::router_with_identity(&settings, Some(service.clone())).unwrap();
  let package = fixture::package("sample", "1.0.0");
  let token = service
    .exchange(exchange_request(
      &signed(&claims("before-disconnect")),
      "sample",
      apell_package::digest(&package),
    ))
    .await
    .unwrap();
  service
    .store
    .disconnect_publisher("sample", owner.github_id)
    .unwrap();
  let (parts, _) = fixture::upload(&token.token, &package).into_parts();
  let never_read = Body::from_stream(futures_util::stream::poll_fn(|_| {
    panic!("A revoked workflow must be rejected before reading an upload");
    #[allow(unreachable_code)]
    std::task::Poll::Ready(None::<std::result::Result<Vec<u8>, std::io::Error>>)
  }));
  let denied = app
    .clone()
    .oneshot(HttpRequest::from_parts(parts, never_read))
    .await
    .unwrap();
  assert_eq!(denied.status(), StatusCode::FORBIDDEN);
  assert!(service
    .exchange(exchange_request(
      &signed(&claims("disconnected")),
      "sample",
      apell_package::digest(&package),
    ))
    .await
    .is_err());

  service
    .store
    .set_publisher("sample", owner.github_id, config().publishers.remove(0))
    .unwrap();
  // Reconnecting the same workflow must never reactivate a previously issued token.
  assert_eq!(
    app
      .clone()
      .oneshot(fixture::upload(&token.token, &package))
      .await
      .unwrap()
      .status(),
    StatusCode::FORBIDDEN
  );
  let token = service
    .exchange(exchange_request(
      &signed(&claims("new-connection")),
      "sample",
      apell_package::digest(&package),
    ))
    .await
    .unwrap();
  let (parts, body) = fixture::upload(&token.token, &package).into_parts();
  let bytes = body.collect().await.unwrap().to_bytes();
  let store = service.store.clone();
  let body = Body::from_stream(futures_util::stream::once(async move {
    store
      .disconnect_publisher("sample", owner.github_id)
      .unwrap();
    Ok::<_, std::io::Error>(bytes)
  }));
  assert_eq!(
    app
      .oneshot(HttpRequest::from_parts(parts, body))
      .await
      .unwrap()
      .status(),
    StatusCode::FORBIDDEN
  );
  assert!(service.store.catalog().unwrap().applets.is_empty());
}

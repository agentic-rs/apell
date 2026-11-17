use super::*;
use crate::{
  auth::{hash, now, tests::config, Auth},
  Store,
};
use axum::http::HeaderMap;

fn fixture() -> (tempfile::TempDir, Store, User) {
  let directory = tempfile::tempdir().unwrap();
  let store = Store::open(directory.path()).unwrap();
  let user = User {
    github_id: 42,
    github_login: "example-user".into(),
  };
  store.upsert_user(&user).unwrap();
  (directory, store, user)
}

fn credential(store: &Store, user: &User, time: i64) -> Authorized {
  let flow = store.start_cli_authorization(time).unwrap();
  store
    .decide_cli_authorization(&flow.user_code, user.github_id, true, time)
    .unwrap();
  let Polled::Authorized(credential) = store
    .poll_cli_authorization(&flow.device_code, time)
    .unwrap()
  else {
    panic!("approval must issue a credential")
  };
  credential
}

#[test]
fn approval_is_single_use_throttled_and_persists_only_hashes() {
  let (_directory, store, user) = fixture();
  let started = store.start_cli_authorization(1000).unwrap();
  assert_eq!(started.expires_at, 1600);
  assert_eq!(user_code(&started.user_code).unwrap(), started.user_code);
  let saved_device: Vec<u8> = store
    .connection()
    .unwrap()
    .query_row("SELECT device_hash FROM cli_authorizations", [], |row| {
      row.get(0)
    })
    .unwrap();
  assert_eq!(saved_device, hash(&started.device_code));
  assert!(matches!(
    store
      .poll_cli_authorization(&started.device_code, 1000)
      .unwrap(),
    Polled::Pending
  ));
  assert_eq!(
    store
      .poll_cli_authorization(&started.device_code, 1001)
      .unwrap_err()
      .code,
    "slow_down"
  );
  assert_eq!(
    store
      .cli_authorization(&started.user_code, 1001)
      .unwrap()
      .status,
    "pending"
  );
  store
    .decide_cli_authorization(&started.user_code, user.github_id, true, 1001)
    .unwrap();
  assert_eq!(
    store
      .cli_authorization(&started.user_code, 1001)
      .unwrap()
      .status,
    "approved"
  );
  assert!(store
    .decide_cli_authorization(&started.user_code, user.github_id, false, 1001)
    .is_err());
  assert_eq!(
    store
      .poll_cli_authorization(&started.device_code, 1002)
      .unwrap_err()
      .code,
    "slow_down"
  );
  let Polled::Authorized(credential) = store
    .poll_cli_authorization(&started.device_code, 1003)
    .unwrap()
  else {
    panic!("approval must issue a credential")
  };
  assert_eq!(credential.user, user);
  assert_eq!(credential.expires_at, 1003 + CREDENTIAL_SECONDS);
  assert_eq!(credential.token.len(), 74);
  let saved: Vec<u8> = store
    .connection()
    .unwrap()
    .query_row("SELECT token_hash FROM cli_credentials", [], |row| {
      row.get(0)
    })
    .unwrap();
  assert_eq!(saved, hash(&credential.token));
  assert_ne!(saved, credential.token.as_bytes());
  assert_eq!(
    store
      .poll_cli_authorization(&started.device_code, 1003)
      .unwrap_err()
      .code,
    "authorization_expired"
  );
  assert_eq!(
    store.cli_identity(&credential.token, 1004).unwrap().user,
    user
  );
  assert!(store
    .cli_identity(&credential.token, credential.expires_at)
    .is_err());
  let details = store.cli_credentials(user.github_id, 1004).unwrap();
  assert_eq!(details.len(), 1);
  assert_eq!(
    store
      .revoke_cli_credential(43, &details[0].id)
      .unwrap_err()
      .status,
    StatusCode::NOT_FOUND
  );
  store
    .revoke_cli_credential(user.github_id, &details[0].id)
    .unwrap();
  assert!(store.cli_identity(&credential.token, 1004).is_err());
}

#[test]
fn denied_expired_and_bounded_flows_never_issue_credentials() {
  let (_directory, store, user) = fixture();
  let denied = store.start_cli_authorization(1000).unwrap();
  store
    .decide_cli_authorization(&denied.user_code, user.github_id, false, 1001)
    .unwrap();
  assert_eq!(
    store
      .cli_authorization(&denied.user_code, 1001)
      .unwrap()
      .status,
    "denied"
  );
  assert_eq!(
    store
      .poll_cli_authorization(&denied.device_code, 1001)
      .unwrap_err()
      .code,
    "authorization_denied"
  );
  assert_eq!(
    store
      .poll_cli_authorization(&denied.device_code, 1600)
      .unwrap_err()
      .code,
    "authorization_expired"
  );
  assert!(store
    .cli_credentials(user.github_id, 1001)
    .unwrap()
    .is_empty());
  for _ in 0..256 {
    store.start_cli_authorization(2000).unwrap();
  }
  assert_eq!(
    store.start_cli_authorization(2000).unwrap_err().status,
    StatusCode::TOO_MANY_REQUESTS
  );
  store.start_cli_authorization(2600).unwrap();
  for invalid in [
    "",
    "ABCDE-FGHIJ",
    "AAAAA-AAAAA?next=https://example.invalid",
  ] {
    assert!(store.cli_authorization(invalid, 2600).is_err());
  }
}

#[test]
fn credential_limits_and_concurrent_redemption_are_atomic() {
  let (_directory, store, user) = fixture();
  let flow = store.start_cli_authorization(1000).unwrap();
  store
    .decide_cli_authorization(&flow.user_code, user.github_id, true, 1000)
    .unwrap();
  let one = store.clone();
  let code = flow.device_code.clone();
  let thread = std::thread::spawn(move || one.poll_cli_authorization(&code, 1000));
  let other = store.poll_cli_authorization(&flow.device_code, 1000);
  let first = thread.join().unwrap();
  assert_eq!(usize::from(first.is_ok()) + usize::from(other.is_ok()), 1);
  for _ in 0..15 {
    credential(&store, &user, 1000);
  }
  let overflow = store.start_cli_authorization(1000).unwrap();
  store
    .decide_cli_authorization(&overflow.user_code, user.github_id, true, 1000)
    .unwrap();
  assert_eq!(
    store
      .poll_cli_authorization(&overflow.device_code, 1000)
      .unwrap_err()
      .status,
    StatusCode::TOO_MANY_REQUESTS
  );
  assert_eq!(
    store.cli_credentials(user.github_id, 1000).unwrap().len(),
    16
  );
  // An expiry removes old credentials when a fresh approval is redeemed.
  credential(&store, &user, 1000 + CREDENTIAL_SECONDS);
  assert_eq!(
    store
      .cli_credentials(user.github_id, 1000 + CREDENTIAL_SECONDS)
      .unwrap()
      .len(),
    1
  );
}

#[test]
fn bearer_and_browser_auth_are_separate_and_never_fall_back() {
  let (_directory, store, user) = fixture();
  let time = now();
  let credential = credential(&store, &user, time);
  let session = store.create_session(&user, None, time).unwrap();
  let auth = Auth::new(config(), store.clone()).unwrap();
  let mut headers = HeaderMap::new();
  headers.insert(
    "authorization",
    format!("Bearer {}", credential.token).parse().unwrap(),
  );
  assert_eq!(auth.require_mutation(&headers).unwrap(), user);
  assert!(auth.browser_user(&headers).is_err());
  assert!(auth.require_browser_mutation(&headers).is_err());
  headers.insert(
    "cookie",
    format!("__Host-apell_session={session}").parse().unwrap(),
  );
  headers.insert("origin", "https://store.example.invalid".parse().unwrap());
  headers.insert(
    "x-csrf-token",
    crate::auth::csrf_token(&session).parse().unwrap(),
  );
  assert!(auth.browser_user(&headers).is_err());
  headers.insert("authorization", "Bearer invalid".parse().unwrap());
  assert!(auth.user(&headers).is_err());
  assert!(auth.require_mutation(&headers).is_err());
  headers.remove("authorization");
  assert_eq!(auth.require_browser_mutation(&headers).unwrap(), user);
  headers.insert(
    "authorization",
    format!("Bearer {}", credential.token).parse().unwrap(),
  );
  headers.append(
    "authorization",
    format!("Bearer {}", credential.token).parse().unwrap(),
  );
  assert!(auth.user(&headers).is_err());
  store.logout_cli(&credential.token).unwrap();
  assert!(store.cli_identity(&credential.token, time).is_err());
  assert_eq!(store.session_user(&session, time).unwrap(), user);
}

#[test]
fn cli_resume_is_browser_bound_single_use_and_keeps_legacy_flow_schema() {
  let (_directory, store, _user) = fixture();
  let flow = store.start_cli_authorization(1000).unwrap();
  store
    .create_flow_for_cli("state", "browser", "verifier", Some(&flow.user_code), 1000)
    .unwrap();
  assert!(store
    .consume_flow_for_cli("state", "other-browser", 1001)
    .is_err());
  assert_eq!(
    store
      .consume_flow_for_cli("state", "browser", 1001)
      .unwrap(),
    ("verifier".into(), Some(flow.user_code))
  );
  assert!(store
    .consume_flow_for_cli("state", "browser", 1001)
    .is_err());
  let connection = store.connection().unwrap();
  let resumes: i64 = connection
    .query_row("SELECT COUNT(*) FROM auth_cli_resumes", [], |row| {
      row.get(0)
    })
    .unwrap();
  assert_eq!(resumes, 0);
  // The old deployment can still insert its original four-column flow rows after rollback.
  connection
    .execute(
      "INSERT INTO auth_flows VALUES (?1,?2,?3,?4)",
      rusqlite::params![hash("old-state"), hash("browser"), "verifier", 1600],
    )
    .unwrap();
  crate::auth::migrate(&connection).unwrap();
  assert_eq!(
    store.consume_flow("old-state", "browser", 1001).unwrap(),
    "verifier"
  );
}

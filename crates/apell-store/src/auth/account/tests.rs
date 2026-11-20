use super::*;
use serde_json::json;

#[test]
fn issuer_urls_are_canonical_and_absolute() {
  for issuer in [
    "https://account.example.invalid",
    "https://account.example.invalid/",
    "https://account.example.invalid/oidc",
    "http://127.0.0.1:8080/oidc",
  ] {
    assert!(validate_issuer(issuer));
  }
  for issuer in [
    "http://account.example.invalid/oidc",
    "https://user@account.example.invalid/oidc",
    "https://@account.example.invalid/oidc",
    "https://account.example.invalid/oidc?query",
    "https://account.example.invalid/oidc#fragment",
    "https://account.example.invalid/oidc/",
    "https://account.example.invalid/one/../oidc",
    "https://account.example.invalid\\oidc",
    " https://account.example.invalid/oidc",
  ] {
    assert!(!validate_issuer(issuer));
  }
}

#[test]
fn github_binding_uses_the_linked_id_and_cross_checks_the_handle() {
  let user: UserInfo = serde_json::from_value(json!({
    "sub": "fixture-subject",
    "identities": {
      "github": {
        "userId": "12345",
        "details": {"rawData": {"userInfo": {"id": 12345, "login": "fixture-user"}}}
      },
      "google": {"userId": "6789"}
    }
  }))
  .unwrap();
  assert_eq!(
    user.identities.unwrap().github.unwrap().verified().unwrap(),
    GitHubIdentity {
      id: 12345,
      login: Some("fixture-user".into())
    }
  );
  for details in [
    None,
    Some(json!({"rawData": {"userInfo": {"id": 99999, "login": "different-user"}}})),
    Some(json!({"rawData": {"userInfo": {"id": 12345, "login": "malformed login"}}})),
  ] {
    let github = LinkedGitHub {
      user_id: "12345".into(),
      details,
    };
    assert_eq!(
      github.verified().unwrap(),
      GitHubIdentity {
        id: 12345,
        login: None
      }
    );
  }
  for user_id in ["0", "0004", "+10", "github-login", "9223372036854775808"] {
    let github = LinkedGitHub {
      user_id: user_id.into(),
      details: None,
    };
    assert_eq!(
      github.verified().unwrap_err().code,
      "invalid_account_identity"
    );
  }
}

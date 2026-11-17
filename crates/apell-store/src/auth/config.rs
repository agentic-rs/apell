use serde::Deserialize;
use std::{io::Read, path::Path};
use url::Url;

// Deliberately does not implement Debug: this object holds the OAuth client secret.
#[derive(Clone)]
pub struct OAuthConfig {
  pub(super) client_id: String,
  pub(super) client_secret: String,
  pub(super) origin: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
  client_id: String,
  client_secret: String,
}

impl OAuthConfig {
  pub fn from_env() -> Result<Option<Self>, String> {
    let Some(path) = std::env::var_os("APELL_STORE_GITHUB_OAUTH_FILE") else {
      return Ok(None);
    };
    let origin = std::env::var("APELL_STORE_PUBLIC_URL")
      .map_err(|_| "Configure APELL_STORE_PUBLIC_URL for GitHub sign-in.")?;
    Self::load(&origin, Path::new(&path)).map(Some)
  }

  fn load(origin: &str, path: &Path) -> Result<Self, String> {
    let file = std::fs::File::open(path).map_err(|_| "Could not open GitHub OAuth credentials.")?;
    let metadata = file
      .metadata()
      .map_err(|_| "Could not inspect GitHub OAuth credentials.")?;
    if !metadata.is_file() || metadata.len() > 16 * 1024 {
      return Err("GitHub OAuth credentials must be a regular JSON file of at most 16 KiB.".into());
    }
    let mut bytes = Vec::new();
    file
      .take(16 * 1024 + 1)
      .read_to_end(&mut bytes)
      .map_err(|_| "Could not read GitHub OAuth credentials.")?;
    if bytes.len() > 16 * 1024 {
      return Err("GitHub OAuth credentials exceed their size limit.".into());
    }
    let credentials: Credentials =
      serde_json::from_slice(&bytes).map_err(|_| "Invalid GitHub OAuth credentials.")?;
    if credentials.client_id.is_empty()
      || credentials.client_id.len() > 128
      || !credentials
        .client_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
      || credentials.client_secret.is_empty()
      || credentials.client_secret.len() > 512
      || !credentials
        .client_secret
        .bytes()
        .all(|byte| byte.is_ascii_graphic())
    {
      return Err("Invalid GitHub OAuth credentials.".into());
    }
    let origin = canonical_origin(origin)?;
    Ok(Self {
      client_id: credentials.client_id,
      client_secret: credentials.client_secret,
      origin,
    })
  }

  pub(super) fn callback(&self) -> String {
    format!("{}/api/v1/auth/github/callback", self.origin)
  }
}

fn canonical_origin(value: &str) -> Result<String, String> {
  let invalid = || {
    "GitHub sign-in requires a root HTTPS public URL without credentials, query, or fragment."
      .to_owned()
  };
  if value.len() > 2048
    || value
      .chars()
      .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
  {
    return Err(invalid());
  }
  let authority = value.strip_prefix("https://").ok_or_else(invalid)?;
  let end = authority.find(['/', '?', '#']).unwrap_or(authority.len());
  if authority[..end].contains('@') || !matches!(&authority[end..], "" | "/") {
    return Err(invalid());
  }
  let url = Url::parse(value).map_err(|_| invalid())?;
  if url.host_str().is_none()
    || !url.username().is_empty()
    || url.password().is_some()
    || url.query().is_some()
    || url.fragment().is_some()
    || url.path() != "/"
  {
    return Err(invalid());
  }
  Ok(url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn credentials_and_public_origin_fail_closed_without_echoing_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oauth.json");
    std::fs::write(
      &path,
      br#"{"client_id":"test-client","client_secret":"fixture-secret"}"#,
    )
    .unwrap();
    assert_eq!(
      OAuthConfig::load("https://store.example.invalid/", &path)
        .unwrap()
        .origin,
      "https://store.example.invalid"
    );
    for origin in [
      "http://store.example.invalid",
      "https://user@store.example.invalid",
      "https://store.example.invalid/path",
      "https://store.example.invalid?x=1",
      "https://store.example.invalid\\evil",
    ] {
      assert!(OAuthConfig::load(origin, &path).is_err());
    }
    std::fs::write(
      &path,
      br#"{"client_id":"test-client","client_secret":"fixture secret"}"#,
    )
    .unwrap();
    assert!(!OAuthConfig::load("https://store.example.invalid", &path)
      .err()
      .unwrap()
      .contains("fixture"));
  }
}

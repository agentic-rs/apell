use serde::{Deserialize, Serialize};
use std::{collections::HashSet, io::Read, path::Path};
use url::Url;

/// Server-managed authorizations. Repository names and token subjects are not identities.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrustedPublisher {
  pub applet_id: String,
  pub repository_id: u64,
  pub repository_owner_id: u64,
  /// Display only. Signed numeric IDs remain the authorization identity.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub repository: Option<String>,
  pub workflow: String,
  pub allowed_refs: Vec<String>,
  pub event_names: Vec<String>,
  #[serde(default)]
  pub environment: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OidcConfig {
  pub audience: String,
  pub publishers: Vec<TrustedPublisher>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
  publishers: Vec<TrustedPublisher>,
}

impl OidcConfig {
  pub fn from_env() -> Result<Option<Self>, String> {
    let public_url = std::env::var_os("APELL_STORE_PUBLIC_URL");
    let policy_file = std::env::var_os("APELL_STORE_TRUSTED_PUBLISHERS_FILE");
    match (public_url, policy_file) {
      (None, None) => Ok(None),
      (Some(url), None) => Ok(Some(Self {
        audience: canonical_audience(url.to_str().ok_or("Invalid store public URL.")?)?,
        publishers: Vec::new(),
      })),
      (Some(url), Some(file)) => Self::load(
        url.to_str().ok_or("Invalid store public URL.")?,
        Path::new(&file),
      )
      .map(Some),
      _ => Err("Configure APELL_STORE_PUBLIC_URL when importing trusted publishers.".into()),
    }
  }

  pub fn load(public_url: &str, path: &Path) -> Result<Self, String> {
    let file =
      std::fs::File::open(path).map_err(|_| "Could not open the trusted publishers file.")?;
    let metadata = file
      .metadata()
      .map_err(|_| "Could not inspect trusted publishers.")?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
      return Err("Trusted publishers must be a regular JSON file of at most 64 KiB.".into());
    }
    let mut bytes = Vec::new();
    file
      .take(64 * 1024 + 1)
      .read_to_end(&mut bytes)
      .map_err(|_| "Could not read trusted publishers.")?;
    if bytes.len() > 64 * 1024 {
      return Err("Trusted publishers file is too large.".into());
    }
    let policy: PolicyFile =
      serde_json::from_slice(&bytes).map_err(|_| "Invalid trusted publishers JSON.")?;
    let config = Self {
      audience: canonical_audience(public_url)?,
      publishers: policy.publishers,
    };
    config.validate()?;
    Ok(config)
  }

  pub(crate) fn validate(&self) -> Result<(), String> {
    if canonical_audience(&self.audience)? != self.audience || self.publishers.len() > 256 {
      return Err("Invalid trusted publishing configuration.".into());
    }
    let mut applets = HashSet::new();
    for policy in &self.publishers {
      let Some(file) = policy.workflow.strip_prefix(".github/workflows/") else {
        return Err("Trusted workflows must be files in .github/workflows/.".into());
      };
      if !apell_package::valid_name(&policy.applet_id)
        || !applets.insert(&policy.applet_id)
        || policy.repository_id == 0
        || policy.repository_owner_id == 0
        || policy.repository.as_ref().is_some_and(|value| {
          let Some((owner, name)) = value.split_once('/') else {
            return true;
          };
          owner.is_empty()
            || owner.len() > 100
            || name.is_empty()
            || name.len() > 100
            || !owner
              .bytes()
              .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || !name
              .bytes()
              .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
            || matches!(name, "." | "..")
        })
        || file.len() > 128
        || !(file.ends_with(".yml") || file.ends_with(".yaml"))
        || file.starts_with('.')
        || file.contains("..")
        || !file
          .bytes()
          .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        || policy.allowed_refs.is_empty()
        || policy.allowed_refs.len() > 16
        || policy.allowed_refs.iter().any(|value| !valid_ref(value))
        || policy.allowed_refs.iter().collect::<HashSet<_>>().len() != policy.allowed_refs.len()
        || policy.event_names.is_empty()
        || policy.event_names.len() > 3
        || policy
          .event_names
          .iter()
          .any(|name| !matches!(name.as_str(), "push" | "workflow_dispatch" | "release"))
        || policy.event_names.iter().collect::<HashSet<_>>().len() != policy.event_names.len()
        || policy.environment.as_ref().is_some_and(|name| {
          name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control)
        })
      {
        return Err("Invalid trusted publisher. Use unique applet IDs, positive repository IDs, an exact workflow file, exact branch/tag refs, and allowed publish events.".into());
      }
    }
    Ok(())
  }
}

pub(super) fn canonical_audience(value: &str) -> Result<String, String> {
  let invalid = || {
    "The store public URL must be a root HTTPS URL without credentials, query, or fragment."
      .to_owned()
  };
  if value.is_empty()
    || value.len() > 2048
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
  if url.scheme() != "https"
    || url.host_str().is_none()
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

pub(super) fn valid_ref(value: &str) -> bool {
  let branch = value
    .strip_prefix("refs/heads/")
    .or_else(|| value.strip_prefix("refs/tags/"));
  value.len() <= 512
    && branch.is_some_and(|name| {
      !name.is_empty()
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.ends_with('.')
        && !name
          .split('/')
          .any(|part| part.starts_with('.') || part.ends_with(".lock"))
    })
    && !value.contains("..")
    && !value.contains("//")
    && !value.contains("@{")
    && !value.chars().any(|c| {
      c.is_whitespace() || c.is_control() || matches!(c, '\\' | '?' | '*' | '[' | ':' | '~' | '^')
    })
}

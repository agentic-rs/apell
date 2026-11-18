use crate::error::{Result, StoreError};
use serde::{Deserialize, Serialize};

pub const MAX_DESCRIPTION_BYTES: usize = 2_000;
pub const MAX_RELEASE_NOTES_BYTES: usize = 10_000;
pub const MAX_METADATA_BYTES: usize = 16 * 1024;
pub const MAX_ICON_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublishMetadata {
  pub description: String,
  #[serde(default)]
  pub project_url: Option<String>,
  #[serde(default)]
  pub release_notes: String,
}

impl PublishMetadata {
  pub fn validate(&self) -> Result<()> {
    if self.description.trim().is_empty()
      || self.description.len() > MAX_DESCRIPTION_BYTES
      || self.description.chars().any(char::is_control)
    {
      return Err(StoreError::invalid(
        "Description must contain 1–2,000 bytes without control characters.",
      ));
    }
    if self.release_notes.len() > MAX_RELEASE_NOTES_BYTES
      || self
        .release_notes
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
      return Err(StoreError::invalid(
        "Release notes exceed 10,000 bytes or contain unsupported control characters.",
      ));
    }
    if let Some(value) = &self.project_url {
      let url = url::Url::parse(value)
        .map_err(|_| StoreError::invalid("Project URL must be an HTTPS URL."))?;
      if value.len() > 2_048
        || value.chars().any(char::is_control)
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
      {
        return Err(StoreError::invalid(
          "Project URL must use HTTPS and cannot include credentials.",
        ));
      }
    }
    Ok(())
  }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
  pub version: String,
  pub sha256: String,
  pub size_bytes: u64,
  pub published_at: String,
  pub release_notes: String,
  pub api_version: u32,
  pub permissions: Vec<String>,
  pub network: bool,
  pub has_backend: bool,
  pub download_url: String,
  pub prerelease: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Applet {
  pub applet_id: String,
  pub name: String,
  pub description: String,
  pub project_url: Option<String>,
  pub icon_url: Option<String>,
  pub latest_release: Release,
  pub latest_stable_release: Option<Release>,
  #[serde(default)]
  pub latest_nightly_release: Option<Release>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Catalog {
  pub applets: Vec<Applet>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct AppletDetail {
  pub applet: Applet,
  pub releases: Vec<Release>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct PublishResult {
  pub applet_id: String,
  pub version: String,
  pub sha256: String,
  pub created: bool,
}

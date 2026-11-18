use crate::{store_client::normalize_store_url, valid_name};
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubSource {
  pub repository_id: u64,
  pub repository: String,
  pub asset_name: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub release_tag: Option<String>,
  #[serde(default)]
  pub prerelease: bool,
}

impl GitHubSource {
  pub(crate) fn validate(&self) -> Result<(), String> {
    if self.repository_id == 0
      || !crate::github::valid_repository(&self.repository)
      || !crate::github::valid_asset_name(&self.asset_name)
      || self
        .release_tag
        .as_ref()
        .is_some_and(|tag| !crate::github::valid_release_tag(tag))
      || (self.prerelease && self.release_tag.is_none())
    {
      return Err("Invalid GitHub package source.".into());
    }
    Ok(())
  }

  fn same_release_asset(&self, other: &Self) -> bool {
    self.repository_id == other.repository_id
      && self.asset_name == other.asset_name
      && self.release_tag == other.release_tag
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreChannel {
  Stable,
  #[serde(alias = "preview")]
  Nightly,
}

impl StoreChannel {
  pub fn from_prerelease(prerelease: bool) -> Self {
    if prerelease {
      Self::Nightly
    } else {
      Self::Stable
    }
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoreSource {
  pub store_url: String,
  pub applet_id: String,
  pub channel: StoreChannel,
}

impl StoreSource {
  pub fn validate(&self) -> Result<(), String> {
    if !valid_name(&self.applet_id) || normalize_store_url(&self.store_url)? != self.store_url {
      return Err("Invalid store package source.".into());
    }
    Ok(())
  }

  pub fn same_identity(&self, other: &Self) -> bool {
    self.store_url == other.store_url && self.applet_id == other.applet_id
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackageSource {
  Github(GitHubSource),
  Store(StoreSource),
}

impl<'de> Deserialize<'de> for PackageSource {
  fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    #[derive(Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    enum TaggedSource {
      Github(GitHubSource),
      Store(StoreSource),
    }
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StoredSource {
      Tagged(TaggedSource),
      LegacyGithub(GitHubSource),
    }
    Ok(match StoredSource::deserialize(deserializer)? {
      StoredSource::Tagged(TaggedSource::Github(source)) | StoredSource::LegacyGithub(source) => {
        Self::Github(source)
      }
      StoredSource::Tagged(TaggedSource::Store(source)) => Self::Store(source),
    })
  }
}

impl From<GitHubSource> for PackageSource {
  fn from(source: GitHubSource) -> Self {
    Self::Github(source)
  }
}

impl From<StoreSource> for PackageSource {
  fn from(source: StoreSource) -> Self {
    Self::Store(source)
  }
}

impl PackageSource {
  pub fn validate(&self) -> Result<(), String> {
    match self {
      Self::Github(source) => source.validate(),
      Self::Store(source) => source.validate(),
    }
  }

  pub fn same_identity(&self, other: &Self) -> bool {
    match (self, other) {
      (Self::Github(left), Self::Github(right)) => left.same_release_asset(right),
      (Self::Store(left), Self::Store(right)) => left.same_identity(right),
      _ => false,
    }
  }

  pub(super) fn validate_equal_version(&self, next: &Self, changed: bool) -> Result<(), String> {
    if let (Self::Github(prior), Self::Github(incoming)) = (self, next) {
      // Promotion to a prerelease cannot make an existing stable version mutable.
      if !prior.prerelease && incoming.prerelease {
        return Err(
          "A stable release cannot become a prerelease without increasing its version.".into(),
        );
      }
      if prior.prerelease
        && incoming.prerelease
        && prior.release_tag.is_some()
        && prior.same_release_asset(incoming)
      {
        return Ok(());
      }
    }
    if changed {
      return Err("This release changed content without increasing its version.".into());
    }
    Ok(())
  }
}

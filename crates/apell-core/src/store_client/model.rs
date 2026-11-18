use crate::bundles::{Manifest, API_VERSION, MAX_BUNDLE_BYTES};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct StoreRelease {
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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoreApplet {
  pub applet_id: String,
  pub name: String,
  pub description: String,
  pub project_url: Option<String>,
  pub icon_url: Option<String>,
  pub latest_release: StoreRelease,
  pub latest_stable_release: Option<StoreRelease>,
  #[serde(default)]
  pub latest_nightly_release: Option<StoreRelease>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoreCatalog {
  pub applets: Vec<StoreApplet>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoreDetail {
  pub applet: StoreApplet,
  pub releases: Vec<StoreRelease>,
}

pub(super) fn lowercase_hash(value: &str) -> bool {
  value.len() == 64
    && value
      .bytes()
      .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn icon_hash(path: &str) -> Result<&str, String> {
  path
    .strip_prefix("/media/")
    .and_then(|value| value.strip_suffix(".png"))
    .filter(|value| lowercase_hash(value))
    .ok_or_else(|| "The store returned an invalid icon location.".into())
}

impl StoreRelease {
  pub fn compare_version(&self, installed: &str) -> Result<std::cmp::Ordering, String> {
    let version = semver::Version::parse(&self.version).map_err(|_| "Invalid store version.")?;
    let installed = semver::Version::parse(installed)
      .map_err(|_| "The installed version is not a semantic version.")?;
    Ok(version.cmp_precedence(&installed))
  }

  pub fn validate(&self, applet_id: &str) -> Result<(), String> {
    let version = semver::Version::parse(&self.version)
      .map_err(|_| "The store returned an invalid version.")?;
    let manifest = Manifest {
      schema_version: 1,
      api_version: self.api_version,
      applet_id: applet_id.to_owned(),
      name: "Store release".into(),
      version: self.version.clone(),
      permissions: self.permissions.clone(),
      network: self.network,
      lib: self.has_backend.then(|| "lib/main.wasm".into()),
    };
    apell_package::validate_manifest(&manifest, false)?;
    if self.api_version != API_VERSION
      || !lowercase_hash(&self.sha256)
      || self.size_bytes == 0
      || self.size_bytes > MAX_BUNDLE_BYTES as u64
      || self.prerelease != !version.pre.is_empty()
      || self.download_url
        != format!(
          "/api/v1/applets/{applet_id}/versions/{}/download",
          self.version
        )
      || self.published_at.is_empty()
      || self.published_at.len() > 64
      || self.published_at.chars().any(char::is_control)
      || self.release_notes.len() > 10_000
      || self
        .release_notes
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
      return Err("The store returned invalid release metadata.".into());
    }
    Ok(())
  }
}

impl StoreApplet {
  /// Older Store catalogs only expose a nightly head when it also happens
  /// to be the newest release overall.
  pub fn nightly_release(&self) -> Option<&StoreRelease> {
    self.latest_nightly_release.as_ref().or_else(|| {
      self
        .latest_release
        .prerelease
        .then_some(&self.latest_release)
    })
  }

  pub fn validate(&self) -> Result<(), String> {
    if !crate::valid_name(&self.applet_id)
      || self.name.trim().is_empty()
      || self.name.len() > 100
      || self.name.chars().any(char::is_control)
      || self.description.trim().is_empty()
      || self.description.len() > 2_000
      || self.description.chars().any(char::is_control)
    {
      return Err("The store returned invalid applet metadata.".into());
    }
    if let Some(project) = &self.project_url {
      let url = url::Url::parse(project).map_err(|_| "Invalid applet project link.")?;
      if project.len() > 2_048
        || project.chars().any(char::is_control)
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
      {
        return Err("Invalid applet project link.".into());
      }
    }
    if let Some(icon) = &self.icon_url {
      icon_hash(icon)?;
    }
    self.latest_release.validate(&self.applet_id)?;
    if let Some(stable) = &self.latest_stable_release {
      stable.validate(&self.applet_id)?;
      if stable.prerelease
        || stable
          .compare_version(&self.latest_release.version)?
          .is_gt()
      {
        return Err("The store returned inconsistent stable release metadata.".into());
      }
    }
    if !self.latest_release.prerelease
      && self.latest_stable_release.as_ref() != Some(&self.latest_release)
    {
      return Err("The store returned inconsistent latest release metadata.".into());
    }
    if let Some(nightly) = &self.latest_nightly_release {
      nightly.validate(&self.applet_id)?;
      if !nightly.prerelease
        || nightly
          .compare_version(&self.latest_release.version)?
          .is_gt()
        || (self.latest_release.prerelease && nightly != &self.latest_release)
      {
        return Err("The store returned inconsistent nightly release metadata.".into());
      }
    }
    Ok(())
  }
}

impl StoreCatalog {
  pub fn validate(&self) -> Result<(), String> {
    if self.applets.len() > 256 {
      return Err("The store catalog is too large.".into());
    }
    let mut ids = HashSet::new();
    for applet in &self.applets {
      applet.validate()?;
      if !ids.insert(&applet.applet_id) {
        return Err("The store returned duplicate applet identities.".into());
      }
    }
    Ok(())
  }
}

impl StoreDetail {
  /// Fill the independent nightly head for details from an older Store.
  pub(super) fn validate_and_fill_nightly_release(
    &mut self,
    requested_id: &str,
  ) -> Result<(), String> {
    let nightly = self.validate_history(requested_id)?;
    if self.applet.latest_nightly_release.is_none() {
      self.applet.latest_nightly_release = nightly.cloned();
    }
    Ok(())
  }

  pub fn validate(&self, requested_id: &str) -> Result<(), String> {
    self.validate_history(requested_id).map(|_| ())
  }

  fn validate_history(&self, requested_id: &str) -> Result<Option<&StoreRelease>, String> {
    self.applet.validate()?;
    if self.applet.applet_id != requested_id
      || self.releases.is_empty()
      || self.releases.len() > 1024
    {
      return Err("The store returned a different applet or invalid version history.".into());
    }
    let mut versions = HashSet::new();
    let mut latest: Option<&StoreRelease> = None;
    let mut stable: Option<&StoreRelease> = None;
    let mut nightly: Option<&StoreRelease> = None;
    for release in &self.releases {
      release.validate(requested_id)?;
      let mut version =
        semver::Version::parse(&release.version).map_err(|_| "Invalid store version.")?;
      version.build = semver::BuildMetadata::EMPTY;
      if !versions.insert(version) {
        return Err("The store returned ambiguous versions.".into());
      }
      if latest.is_none_or(|prior| {
        release
          .compare_version(&prior.version)
          .is_ok_and(|order| order.is_gt())
      }) {
        latest = Some(release);
      }
      let head = if release.prerelease {
        &mut nightly
      } else {
        &mut stable
      };
      if head.is_none_or(|prior| {
        release
          .compare_version(&prior.version)
          .is_ok_and(|order| order.is_gt())
      }) {
        *head = Some(release);
      }
    }
    if latest != Some(&self.applet.latest_release)
      || stable != self.applet.latest_stable_release.as_ref()
      || self
        .applet
        .latest_nightly_release
        .as_ref()
        .is_some_and(|listed| Some(listed) != nightly)
    {
      return Err("The store version history does not match its listing.".into());
    }
    Ok(nightly)
  }
}

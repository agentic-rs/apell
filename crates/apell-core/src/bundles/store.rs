use super::{
  atomic_write, digest, parse, read_version, valid_hash, validate_manifest, write_version, Bundle,
  Manifest, MAX_BUNDLE_BYTES,
};
use crate::valid_name;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read, path::PathBuf};

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

#[derive(Clone, Debug, Serialize)]
pub struct InstalledApplet {
  #[serde(flatten)]
  pub manifest: Manifest,
  pub sha256: String,
  pub source: Option<GitHubSource>,
}

/// A failed installed package remains on disk and is reported independently.
#[derive(Clone, Debug, Serialize)]
pub struct UnavailableApplet {
  pub applet_id: String,
  pub error: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AppletListReport {
  pub applets: Vec<InstalledApplet>,
  pub unavailable: Vec<UnavailableApplet>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Active {
  applet_id: String,
  sha256: String,
  source: Option<GitHubSource>,
  #[serde(skip)]
  legacy: bool,
}

impl Active {
  fn installed(&self, manifest: Manifest) -> InstalledApplet {
    InstalledApplet {
      manifest,
      sha256: self.sha256.clone(),
      source: self.source.clone(),
    }
  }
}

pub struct BundleStore {
  root: PathBuf,
}

impl BundleStore {
  pub fn new(root: PathBuf) -> Self {
    Self { root }
  }

  pub fn install(&self, bytes: &[u8], expected_hash: &str) -> Result<InstalledApplet, String> {
    self.install_with_source(bytes, Some(expected_hash), None)
  }

  pub fn install_with_source(
    &self,
    bytes: &[u8],
    expected_hash: Option<&str>,
    source: Option<GitHubSource>,
  ) -> Result<InstalledApplet, String> {
    self.publish(bytes, expected_hash, source, None)
  }

  pub fn update_from_source(
    &self,
    applet_id: &str,
    bytes: &[u8],
    expected_hash: Option<&str>,
    source: GitHubSource,
  ) -> Result<InstalledApplet, String> {
    self.publish(bytes, expected_hash, Some(source), Some(applet_id))
  }

  fn publish(
    &self,
    bytes: &[u8],
    expected_hash: Option<&str>,
    source: Option<GitHubSource>,
    update_id: Option<&str>,
  ) -> Result<InstalledApplet, String> {
    if bytes.len() > MAX_BUNDLE_BYTES {
      return Err("Package exceeds the 24 MiB archive limit.".into());
    }
    let hash = digest(bytes);
    if expected_hash.is_some_and(|expected| !hash.eq_ignore_ascii_case(expected)) {
      return Err("Package checksum does not match. The installed version was preserved.".into());
    }
    if let Some(source) = &source {
      source.validate()?;
    }
    let manifest = parse(bytes)?.manifest;
    let id = &manifest.applet_id;
    if update_id.is_some_and(|expected| expected != id) {
      return Err("The release package has a different applet ID.".into());
    }
    std::fs::create_dir_all(&self.root).map_err(|_| "Could not create package storage.")?;
    // Cover the source checks and pointer replacement with the same file lock,
    // including distinct store instances or processes opening this directory.
    let lock = std::fs::OpenOptions::new()
      .create(true)
      .truncate(false)
      .read(true)
      .write(true)
      .open(self.root.join(".install-lock"))
      .map_err(|_| "Could not open package installation lock.")?;
    lock.lock().map_err(|_| "Could not lock package storage.")?;
    let directory = self.root.join(id);
    let previous = self.active(id)?;
    if update_id.is_some()
      && !previous
        .as_ref()
        .and_then(|record| record.source.as_ref())
        .zip(source.as_ref())
        .is_some_and(|(old, new)| old.same_release_asset(new))
    {
      return Err("The applet's installation source changed. Reopen its update.".into());
    }
    let mut source = source;
    if let Some(incoming) = &source {
      for candidate in self.ids()? {
        if &candidate != id
          && self
            .active(&candidate)?
            .and_then(|record| record.source)
            .is_some_and(|bound| bound.same_release_asset(incoming))
        {
          return Err("This release asset is already bound to a different applet ID.".into());
        }
      }
    }
    if let Some(old) = &previous {
      if let Some(incoming) = &source {
        if old
          .source
          .as_ref()
          .is_none_or(|prior| !prior.same_release_asset(incoming))
        {
          return Err("This applet ID belongs to a different installation source.".into());
        }
      } else if hash == old.sha256 {
        source = old.source.clone();
      }
      // Read the current immutable snapshot before replacing it. A migration
      // from a legacy non-semver version can proceed only through local install.
      let old_package = self.read_active(old)?;
      if let Ok(prior) = semver::Version::parse(&old_package.manifest.version) {
        let next =
          semver::Version::parse(&manifest.version).map_err(|_| "Invalid package version.")?;
        match next.cmp_precedence(&prior) {
          std::cmp::Ordering::Less => return Err("A newer version is already installed.".into()),
          std::cmp::Ordering::Equal => {
            if let Some(incoming) = &source {
              let prior = old
                .source
                .as_ref()
                .ok_or("Invalid installed package source.")?;
              // A metadata-only promotion must not make a previously stable
              // version replaceable by a second update with different bytes.
              if !prior.prerelease && incoming.prerelease {
                return Err(
                  "A stable release cannot become a prerelease without increasing its version."
                    .into(),
                );
              }
              let rolling_prerelease = prior.prerelease
                && incoming.prerelease
                && prior.release_tag.is_some()
                && prior.same_release_asset(incoming);
              if hash != old.sha256 && !rolling_prerelease {
                return Err("This release changed content without increasing its version.".into());
              }
            }
          }
          _ => {}
        }
      }
    }
    let active = Active {
      applet_id: id.clone(),
      sha256: hash,
      source,
      legacy: false,
    };
    std::fs::create_dir_all(&directory).map_err(|_| "Could not create applet storage.")?;
    write_version(
      &directory.join(format!("{}.apel", active.sha256)),
      bytes,
      &active.sha256,
    )?;
    let pointer = serde_json::to_vec(&active).map_err(|_| "Could not encode package source.")?;
    let path = directory.join("active");
    if let Err(error) = atomic_write(&path, &pointer) {
      // Directory sync can fail after rename. The visible pointer already
      // selects this immutable version if its complete bytes match.
      if !std::fs::read(path).is_ok_and(|visible| visible == pointer) {
        return Err(error);
      }
    }
    Ok(active.installed(manifest))
  }

  /// Snapshot the active pointer once; a concurrent install cannot mix a
  /// manifest, source and content hash from different package versions.
  pub fn installed(&self, applet_id: &str) -> Result<InstalledApplet, String> {
    let active = self.active(applet_id)?.ok_or("Applet is not installed.")?;
    Ok(active.installed(self.read_active(&active)?.manifest))
  }

  pub fn load(&self, applet_id: &str) -> Result<Bundle, String> {
    let active = self.active(applet_id)?.ok_or("Applet is not installed.")?;
    self.read_active(&active)
  }

  pub fn load_with_metadata(&self, applet_id: &str) -> Result<(Bundle, InstalledApplet), String> {
    let active = self.active(applet_id)?.ok_or("Applet is not installed.")?;
    let bundle = self.read_active(&active)?;
    let installed = active.installed(bundle.manifest.clone());
    Ok((bundle, installed))
  }

  pub fn list(&self) -> Result<Vec<InstalledApplet>, String> {
    let mut installed = Vec::new();
    for id in self.ids()? {
      if let Some(record) = self.active(&id)? {
        installed.push(record.installed(self.read_active(&record)?.manifest));
      }
    }
    sort_installed(&mut installed);
    Ok(installed)
  }

  /// List healthy applets while reporting each unreadable or incompatible package.
  /// This never changes active pointers, package bytes, or applet-owned data.
  /// Store-wide access failures remain errors; one broken package is isolated.
  pub fn list_report(&self) -> Result<AppletListReport, String> {
    let mut report = AppletListReport {
      applets: Vec::new(),
      unavailable: Vec::new(),
    };
    for id in self.ids()? {
      let result = self.active(&id).and_then(|record| {
        record
          .map(|active| {
            self
              .read_active(&active)
              .map(|bundle| active.installed(bundle.manifest))
          })
          .transpose()
      });
      match result {
        Ok(Some(applet)) => report.applets.push(applet),
        Ok(None) => {}
        Err(error) => report.unavailable.push(UnavailableApplet {
          applet_id: id,
          error,
        }),
      }
    }
    sort_installed(&mut report.applets);
    report
      .unavailable
      .sort_by(|a, b| a.applet_id.cmp(&b.applet_id));
    Ok(report)
  }

  fn ids(&self) -> Result<Vec<String>, String> {
    let entries = match std::fs::read_dir(&self.root) {
      Ok(entries) => entries,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
      Err(_) => return Err("Could not list applets.".into()),
    };
    let mut ids = Vec::new();
    for entry in entries {
      let entry = entry.map_err(|_| "Could not read applet entry.")?;
      let id = entry.file_name().to_string_lossy().into_owned();
      if valid_name(&id)
        && entry
          .file_type()
          .map_err(|_| "Could not inspect applet entry.")?
          .is_dir()
      {
        ids.push(id);
      }
    }
    Ok(ids)
  }

  fn active(&self, id: &str) -> Result<Option<Active>, String> {
    if !valid_name(id) {
      return Err("Invalid applet identifier.".into());
    }
    let file = match std::fs::File::open(self.root.join(id).join("active")) {
      Ok(file) => file,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
      Err(_) => return Err("Could not read installed package pointer.".into()),
    };
    let mut bytes = Vec::new();
    file
      .take(4097)
      .read_to_end(&mut bytes)
      .map_err(|_| "Could not read installed package pointer.")?;
    if bytes.len() > 4096 {
      return Err("Installed package pointer is invalid.".into());
    }
    if let Some(hash) = std::str::from_utf8(&bytes)
      .ok()
      .filter(|value| valid_hash(value))
    {
      return Ok(Some(Active {
        applet_id: id.to_owned(),
        sha256: hash.to_owned(),
        source: None,
        legacy: true,
      }));
    }
    let record: Active =
      serde_json::from_slice(&bytes).map_err(|_| "Installed package pointer is invalid.")?;
    if record.applet_id != id || !valid_hash(&record.sha256) {
      return Err("Installed package pointer is invalid.".into());
    }
    if let Some(source) = &record.source {
      source.validate()?;
    }
    Ok(Some(record))
  }

  fn read_active(&self, active: &Active) -> Result<Bundle, String> {
    let extension = if active.legacy { "json" } else { "apel" };
    let path = self
      .root
      .join(&active.applet_id)
      .join(format!("{}.{}", active.sha256, extension));
    let bytes = read_version(&path, &active.sha256)?;
    let bundle = if active.legacy {
      legacy_bundle(&bytes)?
    } else {
      parse(&bytes)?
    };
    if bundle.manifest.applet_id != active.applet_id {
      return Err("Installed package identity mismatch.".into());
    }
    Ok(bundle)
  }
}

fn sort_installed(applets: &mut [InstalledApplet]) {
  applets.sort_by(|a, b| {
    a.manifest
      .name
      .cmp(&b.manifest.name)
      .then_with(|| a.manifest.applet_id.cmp(&b.manifest.applet_id))
  });
}

fn legacy_bundle(bytes: &[u8]) -> Result<Bundle, String> {
  #[derive(Deserialize)]
  #[serde(deny_unknown_fields)]
  struct OldBundle {
    manifest: Manifest,
    html: String,
  }
  let old: OldBundle =
    serde_json::from_slice(bytes).map_err(|_| "Invalid cached legacy applet.")?;
  validate_manifest(&old.manifest, true)?;
  if old.html.is_empty() {
    return Err("Applet entry is empty.".into());
  }
  let files = BTreeMap::from([("index.html".to_owned(), old.html.as_bytes().to_vec())]);
  Ok(Bundle {
    manifest: old.manifest,
    html: old.html,
    files,
  })
}

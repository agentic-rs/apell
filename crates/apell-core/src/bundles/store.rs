use super::{
  atomic_write, digest, parse, read_version, valid_hash, validate_manifest, write_version, Bundle,
  Manifest, PackageSource, StoreSource, MAX_BUNDLE_BYTES,
};
use crate::valid_name;
use serde::{Deserialize, Serialize};
use std::{
  collections::BTreeMap,
  fs,
  io::Read,
  path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize)]
pub struct InstalledApplet {
  #[serde(flatten)]
  pub manifest: Manifest,
  pub sha256: String,
  pub source: Option<PackageSource>,
  /// Optional package-owned artwork, validated from this exact package snapshot.
  pub icon_data_url: Option<String>,
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
  #[serde(skip)]
  pub retained: BTreeMap<String, StoreSwitchSnapshot>,
}

/// Exact ownership and package state reviewed before changing sources.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoreSwitchSnapshot {
  pub sha256: String,
  #[serde(deserialize_with = "Option::deserialize")]
  pub source: Option<PackageSource>,
  pub removed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageRecord {
  applet_id: String,
  sha256: String,
  // An explicit null identifies a file install. A missing source cannot
  // establish ownership of retained data.
  #[serde(deserialize_with = "Option::deserialize")]
  source: Option<PackageSource>,
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  removed: bool,
  #[serde(skip)]
  legacy: bool,
}

impl PackageRecord {
  fn switch_snapshot(&self) -> StoreSwitchSnapshot {
    StoreSwitchSnapshot {
      sha256: self.sha256.clone(),
      source: self.source.clone(),
      removed: self.removed,
    }
  }

  fn write(&self, directory: &Path) -> Result<(), String> {
    let pointer = serde_json::to_vec(self).map_err(|_| "Could not encode package source.")?;
    let path = directory.join("active");
    if let Err(error) = atomic_write(&path, &pointer) {
      // Directory sync can fail after rename. The visible pointer still owns
      // the installation if it contains the complete record we wrote.
      if !fs::read(path).is_ok_and(|visible| visible == pointer) {
        return Err(error);
      }
    }
    Ok(())
  }

  fn installed(&self, bundle: &Bundle) -> InstalledApplet {
    InstalledApplet {
      manifest: bundle.manifest.clone(),
      sha256: self.sha256.clone(),
      source: self.source.clone(),
      icon_data_url: bundle
        .read_asset("icon.png")
        .and_then(|bytes| crate::icons::png_data_url(bytes).ok()),
    }
  }
}

#[derive(Clone, Copy)]
enum InstallGuard<'a> {
  Any,
  Update(&'a str),
  StoreSnapshot(Option<&'a str>),
  StoreSwitch(&'a StoreSwitchSnapshot),
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

  // The file lock serializes all changes of installation state, even between
  // distinct BundleStore instances or processes sharing a directory.
  fn lock_storage(&self) -> Result<fs::File, String> {
    fs::create_dir_all(&self.root).map_err(|_| "Could not create package storage.")?;
    if !fs::symlink_metadata(&self.root)
      .map_err(|_| "Could not inspect package storage.")?
      .is_dir()
    {
      return Err("Invalid package storage directory.".into());
    }
    let lock = fs::OpenOptions::new()
      .create(true)
      .truncate(false)
      .read(true)
      .write(true)
      .open(self.root.join(".install-lock"))
      .map_err(|_| "Could not open package installation lock.")?;
    lock.lock().map_err(|_| "Could not lock package storage.")?;
    Ok(lock)
  }

  /// Keep the source binding with the applet's saved data. An unreadable
  /// package can be removed as long as its source record is still intact.
  pub fn remove(
    &self,
    applet_id: &str,
    before_remove: impl FnOnce() -> Result<(), String>,
  ) -> Result<(), String> {
    let _lock = self.lock_storage()?;
    let mut record = self.record(applet_id)?.ok_or("Applet is not installed.")?;
    let directory = self.root.join(applet_id);
    if !record.removed {
      before_remove()?;
      record.removed = true;
      record.write(&directory)?;
    }
    remove_versions(&directory)
  }

  pub fn install_with_source(
    &self,
    bytes: &[u8],
    expected_hash: Option<&str>,
    source: Option<PackageSource>,
  ) -> Result<InstalledApplet, String> {
    self.publish(bytes, expected_hash, source, InstallGuard::Any)
  }

  pub fn update_from_source(
    &self,
    applet_id: &str,
    bytes: &[u8],
    expected_hash: Option<&str>,
    source: PackageSource,
  ) -> Result<InstalledApplet, String> {
    self.publish(
      bytes,
      expected_hash,
      Some(source),
      InstallGuard::Update(applet_id),
    )
  }

  /// Apply only the installed package snapshot that the user reviewed.
  /// The check shares the installation lock with publication and ownership checks.
  pub fn install_store_release(
    &self,
    bytes: &[u8],
    expected_hash: &str,
    source: StoreSource,
    expected_installed_sha256: Option<&str>,
  ) -> Result<InstalledApplet, String> {
    if expected_installed_sha256.is_some_and(|hash| !valid_hash(hash)) {
      return Err("Invalid installed package snapshot.".into());
    }
    self.publish(
      bytes,
      Some(expected_hash),
      Some(source.into()),
      InstallGuard::StoreSnapshot(expected_installed_sha256),
    )
  }

  /// Move an applet and its retained data to a different, explicitly reviewed
  /// Store source. The release must still match the reviewed bytes.
  pub fn switch_to_store_release(
    &self,
    bytes: &[u8],
    expected_hash: &str,
    source: StoreSource,
    expected: &StoreSwitchSnapshot,
  ) -> Result<InstalledApplet, String> {
    if !valid_hash(&expected.sha256) {
      return Err("Invalid installed package snapshot.".into());
    }
    self.publish(
      bytes,
      Some(expected_hash),
      Some(source.into()),
      InstallGuard::StoreSwitch(expected),
    )
  }

  fn publish(
    &self,
    bytes: &[u8],
    expected_hash: Option<&str>,
    source: Option<PackageSource>,
    guard: InstallGuard<'_>,
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
    let bundle = parse(bytes)?;
    let manifest = &bundle.manifest;
    let id = &manifest.applet_id;
    if matches!(guard, InstallGuard::Update(expected) if expected != id)
      || matches!(&source, Some(PackageSource::Store(source)) if source.applet_id != *id)
    {
      return Err("The release package has a different applet ID.".into());
    }
    let _lock = self.lock_storage()?;
    let directory = self.root.join(id);
    let record = self.record(id)?;
    let switch_source = if let InstallGuard::StoreSwitch(expected) = guard {
      let current = record
        .as_ref()
        .ok_or("The applet no longer has an installation source.")?;
      if &current.switch_snapshot() != expected {
        return Err(
          "The installed applet changed. Reopen its store page before switching sources.".into(),
        );
      }
      if current
        .source
        .as_ref()
        .zip(source.as_ref())
        .is_some_and(|(old, new)| old.same_identity(new))
      {
        return Err("The applet already uses this Store source.".into());
      }
      true
    } else {
      false
    };
    let retained = record.as_ref().filter(|record| record.removed);
    if let Some(retained) = retained.filter(|_| !switch_source) {
      let same_source = match (&retained.source, &source) {
        (None, None) => true,
        (Some(old), Some(new)) => old.same_identity(new),
        _ => false,
      };
      if !same_source {
        return Err("This applet has saved data. Reinstall it from its original source.".into());
      }
    }
    let previous = record.as_ref().filter(|record| !record.removed);
    if matches!(guard, InstallGuard::Update(_))
      && !previous
        .and_then(|record| record.source.as_ref())
        .zip(source.as_ref())
        .is_some_and(|(old, new)| old.same_identity(new))
    {
      return Err("The applet's installation source changed. Reopen its update.".into());
    }
    if let InstallGuard::StoreSnapshot(expected) = guard {
      let matches = match (expected, previous) {
        (None, None) => true,
        (Some(hash), Some(active)) => {
          active.sha256 == hash
            && active
              .source
              .as_ref()
              .zip(source.as_ref())
              .is_some_and(|(old, new)| old.same_identity(new))
        }
        _ => false,
      };
      if !matches {
        return Err(
          "The installed applet changed. Reopen its store page before installing.".into(),
        );
      }
    }
    let mut source = source;
    // Store ownership includes the manifest applet ID, so only GitHub assets
    // need a cross-ID check. Unrelated damaged pointers must not block stores.
    if let Some(incoming @ PackageSource::Github(_)) = &source {
      for candidate in self.ids()? {
        if &candidate != id
          && self
            .record(&candidate)?
            .and_then(|record| record.source)
            .is_some_and(|bound| bound.same_identity(incoming))
        {
          return Err("This release asset is already bound to a different applet ID.".into());
        }
      }
    }
    if let Some(old) = previous {
      if let Some(incoming) = &source {
        if !switch_source
          && old
            .source
            .as_ref()
            .is_none_or(|prior| !prior.same_identity(incoming))
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
          std::cmp::Ordering::Equal if !switch_source => {
            if let Some(incoming) = &source {
              let prior = old
                .source
                .as_ref()
                .ok_or("Invalid installed package source.")?;
              prior.validate_equal_version(incoming, hash != old.sha256)?;
            }
          }
          _ => {}
        }
      } else if switch_source {
        return Err("The installed applet version cannot be compared to the Store release.".into());
      }
    }
    let active = PackageRecord {
      applet_id: id.clone(),
      sha256: hash,
      source,
      removed: false,
      legacy: false,
    };
    // Retry a removal interrupted after the source reservation was persisted.
    if retained.is_some() {
      remove_versions(&directory)?;
    }
    std::fs::create_dir_all(&directory).map_err(|_| "Could not create applet storage.")?;
    write_version(
      &directory.join(format!("{}.apel", active.sha256)),
      bytes,
      &active.sha256,
    )?;
    active.write(&directory)?;
    Ok(active.installed(&bundle))
  }

  /// Snapshot the active pointer once; a concurrent install cannot mix a
  /// manifest, icon, source and content hash from different package versions.
  pub fn installed(&self, applet_id: &str) -> Result<InstalledApplet, String> {
    let active = self.active(applet_id)?.ok_or("Applet is not installed.")?;
    Ok(active.installed(&self.read_active(&active)?))
  }

  pub fn load(&self, applet_id: &str) -> Result<Bundle, String> {
    let active = self.active(applet_id)?.ok_or("Applet is not installed.")?;
    self.read_active(&active)
  }

  pub fn load_with_metadata(&self, applet_id: &str) -> Result<(Bundle, InstalledApplet), String> {
    let active = self.active(applet_id)?.ok_or("Applet is not installed.")?;
    let bundle = self.read_active(&active)?;
    let installed = active.installed(&bundle);
    Ok((bundle, installed))
  }

  pub fn list(&self) -> Result<Vec<InstalledApplet>, String> {
    let mut installed = Vec::new();
    for id in self.ids()? {
      if let Some(record) = self.active(&id)? {
        installed.push(record.installed(&self.read_active(&record)?));
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
      retained: BTreeMap::new(),
    };
    for id in self.ids()? {
      let result = self.record(&id).and_then(|record| match record {
        Some(active) if !active.removed => self
          .read_active(&active)
          .map(|bundle| Some(active.installed(&bundle))),
        Some(retained) => {
          report
            .retained
            .insert(id.clone(), retained.switch_snapshot());
          Ok(None)
        }
        None => Ok(None),
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

  fn active(&self, id: &str) -> Result<Option<PackageRecord>, String> {
    Ok(self.record(id)?.filter(|record| !record.removed))
  }

  fn record(&self, id: &str) -> Result<Option<PackageRecord>, String> {
    if !valid_name(id) {
      return Err("Invalid applet identifier.".into());
    }
    let directory = self.root.join(id);
    match fs::symlink_metadata(&directory) {
      Ok(meta) if meta.is_dir() => {}
      Ok(_) => return Err("Invalid package storage directory.".into()),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
      Err(_) => return Err("Could not inspect package storage directory.".into()),
    }
    let pointer = directory.join("active");
    match fs::symlink_metadata(&pointer) {
      Ok(meta) if meta.is_file() => {}
      Ok(_) => return Err("Installed package pointer is invalid.".into()),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
      Err(_) => return Err("Could not read installed package pointer.".into()),
    }
    let file = fs::File::open(pointer).map_err(|_| "Could not read installed package pointer.")?;
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
      return Ok(Some(PackageRecord {
        applet_id: id.to_owned(),
        sha256: hash.to_owned(),
        source: None,
        removed: false,
        legacy: true,
      }));
    }
    let record: PackageRecord =
      serde_json::from_slice(&bytes).map_err(|_| "Installed package pointer is invalid.")?;
    if record.applet_id != id || !valid_hash(&record.sha256) {
      return Err("Installed package pointer is invalid.".into());
    }
    if let Some(source) = &record.source {
      source.validate()?;
    }
    if matches!(&record.source, Some(PackageSource::Store(source)) if source.applet_id != id) {
      return Err("Installed package source identity mismatch.".into());
    }
    Ok(Some(record))
  }

  fn read_active(&self, active: &PackageRecord) -> Result<Bundle, String> {
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

fn remove_versions(directory: &Path) -> Result<(), String> {
  // The directory contains only immutable package versions, the active/source
  // record, and any staging files left by interrupted publications.
  for entry in fs::read_dir(directory).map_err(|_| "Could not list package files.")? {
    let entry = entry.map_err(|_| "Could not inspect package file.")?;
    if entry.file_name() != "active" {
      fs::remove_file(entry.path()).map_err(|_| "Could not remove package files.")?;
    }
  }
  super::sync_directory(directory)
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

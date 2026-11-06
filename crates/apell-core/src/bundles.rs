mod archive;
mod store;

use crate::valid_name;
pub use archive::{pack_directory, parse};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
  collections::{BTreeMap, HashSet},
  io::Read,
  path::Path,
};
pub use store::{AppletListReport, BundleStore, GitHubSource, InstalledApplet, UnavailableApplet};

pub const API_VERSION: u32 = 2;
pub const MAX_BUNDLE_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_EXPANDED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
  pub schema_version: u32,
  pub api_version: u32,
  pub applet_id: String,
  pub name: String,
  pub version: String,
  pub permissions: Vec<String>,
  #[serde(default = "network_enabled_by_default")]
  pub network: bool,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub lib: Option<String>,
}

fn network_enabled_by_default() -> bool {
  true
}

#[derive(Clone, Debug)]
pub struct Bundle {
  pub manifest: Manifest,
  pub html: String,
  pub files: BTreeMap<String, Vec<u8>>,
}

impl Bundle {
  pub fn read_asset(&self, path: &str) -> Option<&[u8]> {
    self.files.get(path).map(Vec::as_slice)
  }
}

fn validate_manifest(manifest: &Manifest, legacy: bool) -> Result<(), String> {
  let expected_api = if legacy { 1 } else { API_VERSION };
  if manifest.schema_version != 1 || manifest.api_version != expected_api {
    return Err("Unsupported package or host API version.".into());
  }
  let valid_version = if legacy {
    manifest.lib.is_none()
      && manifest
        .version
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
  } else {
    semver::Version::parse(&manifest.version).is_ok()
  };
  if !valid_name(&manifest.applet_id)
    || manifest.name.trim().is_empty()
    || manifest.name.len() > 100
    || manifest.name.chars().any(char::is_control)
    || manifest.version.is_empty()
    || manifest.version.len() > 80
    || !valid_version
  {
    return Err("Invalid applet metadata or semantic version.".into());
  }
  let allowed = [
    "sqlite",
    "llm",
    "resources",
    "device",
    "http",
    "fs",
    "config",
    "websocket",
    "secrets",
    "crypto",
    "runtime",
    "audio",
    "speech",
  ];
  let mut seen = HashSet::new();
  if manifest
    .permissions
    .iter()
    .any(|p| !allowed.contains(&p.as_str()) || !seen.insert(p))
  {
    return Err("Unsupported or duplicate applet permission.".into());
  }
  Ok(())
}

pub fn digest(bytes: &[u8]) -> String {
  format!("{:x}", Sha256::digest(bytes))
}
pub fn valid_hash(hash: &str) -> bool {
  hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit())
}

fn read_version(path: &Path, expected_hash: &str) -> Result<Vec<u8>, String> {
  let file = std::fs::File::open(path).map_err(|_| "Could not read installed bundle.")?;
  let mut bytes = Vec::new();
  file
    .take(MAX_BUNDLE_BYTES as u64 + 1)
    .read_to_end(&mut bytes)
    .map_err(|_| "Could not read installed bundle.")?;
  if bytes.len() > MAX_BUNDLE_BYTES || digest(&bytes) != expected_hash {
    return Err("Installed bundle failed integrity verification.".into());
  }
  Ok(bytes)
}

fn write_version(path: &Path, bytes: &[u8], hash: &str) -> Result<(), String> {
  use std::io::Write;
  let parent = path.parent().ok_or("Invalid storage location.")?;
  let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
  let result = (|| {
    let mut file = std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&temporary)
      .map_err(|_| "Could not stage bundle.")?;
    file
      .write_all(bytes)
      .and_then(|_| file.sync_all())
      .map_err(|_| "Could not persist bundle.")?;
    drop(file);
    // Publishing a hard link is atomic and never replaces a previously published
    // version. Concurrent identical installs can safely reuse the winner's file.
    match std::fs::hard_link(&temporary, path) {
      Ok(()) => sync_directory(parent),
      Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
        read_version(path, hash).map(|_| ())
      }
      Err(_) => Err("Could not publish bundle version.".into()),
    }
  })();
  let _ = std::fs::remove_file(temporary);
  result
}

fn sync_directory(path: &Path) -> Result<(), String> {
  #[cfg(unix)]
  std::fs::File::open(path)
    .and_then(|directory| directory.sync_all())
    .map_err(|_| "Could not persist storage directory.".to_string())?;
  #[cfg(not(unix))]
  let _ = path;
  Ok(())
}

pub fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
  use std::io::Write;
  let parent = path.parent().ok_or("Invalid storage location.")?;
  std::fs::create_dir_all(parent).map_err(|_| "Could not create storage directory.")?;
  let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
  let result = (|| {
    let mut file = std::fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&temporary)
      .map_err(|_| "Could not stage data.")?;
    file
      .write_all(bytes)
      .and_then(|_| file.sync_all())
      .map_err(|_| "Could not persist data.")?;
    drop(file);
    std::fs::rename(&temporary, path).map_err(|_| "Could not activate data.")?;
    sync_directory(parent)
  })();
  let _ = std::fs::remove_file(temporary);
  result
}

#[cfg(test)]
mod tests;

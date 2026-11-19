//! Shared .apel package format, validation, and deterministic packing.

mod archive;
mod storage;

pub use archive::{pack_directory, parse};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
pub use storage::{atomic_write, sync_directory};

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

pub fn validate_manifest(manifest: &Manifest, legacy: bool) -> Result<(), String> {
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
    "camera",
    "location",
    "photos",
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

pub fn valid_name(value: &str) -> bool {
  !value.is_empty()
    && value.len() <= 80
    && value
      .bytes()
      .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_')
}

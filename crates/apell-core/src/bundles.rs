use crate::valid_name;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
  collections::HashSet,
  io::Read,
  path::{Path, PathBuf},
};

pub const MAX_BUNDLE_BYTES: usize = 24 * 1024 * 1024;
pub const APPLET_CSP: &str = "default-src 'none'; script-src 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
  pub schema_version: u32,
  pub api_version: u32,
  pub applet_id: String,
  pub name: String,
  pub version: String,
  pub permissions: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
  pub manifest: Manifest,
  pub html: String,
}

#[derive(Clone, Serialize)]
pub struct InstalledApplet {
  #[serde(flatten)]
  pub manifest: Manifest,
  pub sha256: String,
}

pub struct BundleStore {
  root: PathBuf,
}

impl BundleStore {
  pub fn new(root: PathBuf) -> Self {
    Self { root }
  }

  pub fn install(&self, bytes: &[u8], expected_hash: &str) -> Result<InstalledApplet, String> {
    if bytes.len() > MAX_BUNDLE_BYTES {
      return Err("Bundle exceeds the 24 MiB limit.".into());
    }
    let hash = digest(bytes);
    if expected_hash.len() != 64 || !hash.eq_ignore_ascii_case(expected_hash) {
      return Err("Bundle checksum does not match. The installed version was preserved.".into());
    }
    let bundle = parse(bytes)?;
    let directory = self.root.join(&bundle.manifest.applet_id);
    std::fs::create_dir_all(&directory).map_err(|_| "Could not create bundle storage.")?;
    // Content addressed immutable versions. Only the tiny active pointer is replaced.
    let version_file = directory.join(format!("{hash}.json"));
    write_version(&version_file, bytes, &hash)?;
    atomic_write(&directory.join("active"), hash.as_bytes())?;
    Ok(InstalledApplet {
      manifest: bundle.manifest,
      sha256: hash,
    })
  }

  pub fn load(&self, applet_id: &str) -> Result<Bundle, String> {
    self
      .snapshot(applet_id)?
      .map(|(bundle, _)| bundle)
      .ok_or_else(|| "Applet is not installed.".into())
  }

  // The active pointer is read exactly once. Immutable version files keep the
  // manifest and checksum consistent even if another install changes the pointer.
  fn snapshot(&self, applet_id: &str) -> Result<Option<(Bundle, String)>, String> {
    if !valid_name(applet_id) {
      return Err("Invalid applet identifier.".into());
    }
    let directory = self.root.join(applet_id);
    let pointer = match std::fs::File::open(directory.join("active")) {
      Ok(file) => file,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
      Err(_) => return Err("Could not read installed bundle pointer.".into()),
    };
    let mut hash = String::new();
    pointer
      .take(65)
      .read_to_string(&mut hash)
      .map_err(|_| "Installed bundle pointer is invalid.")?;
    if !valid_hash(&hash) {
      return Err("Installed bundle pointer is invalid.".into());
    }
    let bytes = read_version(&directory.join(format!("{hash}.json")), &hash)?;
    let bundle = parse(&bytes)?;
    if bundle.manifest.applet_id != applet_id {
      return Err("Installed bundle identity mismatch.".into());
    }
    Ok(Some((bundle, hash)))
  }

  pub fn list(&self) -> Result<Vec<InstalledApplet>, String> {
    if !self.root.exists() {
      return Ok(vec![]);
    }
    let entries = std::fs::read_dir(&self.root).map_err(|_| "Could not list applets.")?;
    let mut applets = Vec::new();
    for entry in entries {
      let entry = entry.map_err(|_| "Could not read applet entry.")?;
      let id = entry.file_name().to_string_lossy().into_owned();
      if !valid_name(&id) || !entry.path().is_dir() {
        continue;
      }
      // A first install creates its directory before it publishes the pointer.
      if let Some((bundle, hash)) = self.snapshot(&id)? {
        applets.push(InstalledApplet {
          manifest: bundle.manifest,
          sha256: hash,
        });
      }
    }
    applets.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    Ok(applets)
  }
}

pub fn parse(bytes: &[u8]) -> Result<Bundle, String> {
  if bytes.len() > MAX_BUNDLE_BYTES {
    return Err("Bundle is too large.".into());
  }
  let bundle: Bundle = serde_json::from_slice(bytes).map_err(|_| "Invalid applet bundle.")?;
  let m = &bundle.manifest;
  if m.schema_version != 1 || m.api_version != 1 {
    return Err("Unsupported bundle or host API version.".into());
  }
  if !valid_name(&m.applet_id)
    || m.name.trim().is_empty()
    || m.name.len() > 100
    || m.name.chars().any(char::is_control)
    || m.version.is_empty()
    || m.version.len() > 40
    || !m
      .version
      .bytes()
      .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
  {
    return Err("Invalid applet metadata.".into());
  }
  let allowed = ["sqlite", "llm", "resources", "device"];
  let mut seen = HashSet::new();
  if m
    .permissions
    .iter()
    .any(|p| !allowed.contains(&p.as_str()) || !seen.insert(p))
  {
    return Err("Unsupported or duplicate applet permission.".into());
  }
  if bundle.html.is_empty() {
    return Err("Applet entry is empty.".into());
  }
  Ok(bundle)
}

pub fn sandbox_document(html: &str) -> String {
  // This policy precedes all applet markup; a later CSP can only tighten it.
  format!(
    "<!doctype html><meta http-equiv=\"Content-Security-Policy\" content=\"{APPLET_CSP}\">{html}"
  )
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
mod tests {
  use super::*;
  fn fixture(id: &str, api: u32) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"manifest":{"schema_version":1,"api_version":api,"applet_id":id,"name":"Test","version":"1","permissions":["sqlite"]},"html":"<p>Test</p>"})).unwrap()
  }
  #[test]
  fn installs_verified_versions_and_preserves_previous_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let store = BundleStore::new(dir.path().into());
    let bytes = fixture("test", 1);
    store.install(&bytes, &digest(&bytes)).unwrap();
    assert!(store.install(b"broken", &digest(&bytes)).is_err());
    assert_eq!(store.load("test").unwrap().html, "<p>Test</p>");
    assert_eq!(store.list().unwrap().len(), 1);
    let newer = fixture("test", 2);
    assert!(store.install(&newer, &digest(&newer)).is_err());
    assert!(store.load("../test").is_err());
  }
  #[test]
  fn detects_tampering_and_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let store = BundleStore::new(dir.path().into());
    let bad = fixture("../escape", 1);
    assert!(store.install(&bad, &digest(&bad)).is_err());
    let bytes = fixture("test", 1);
    store.install(&bytes, &digest(&bytes)).unwrap();
    std::fs::write(
      dir
        .path()
        .join("test")
        .join(format!("{}.json", digest(&bytes))),
      b"bad",
    )
    .unwrap();
    assert!(store.load("test").is_err());
    assert!(store.install(&bytes, &digest(&bytes)).is_err());
  }

  #[test]
  fn listing_skips_unpublished_install_directories() {
    let dir = tempfile::tempdir().unwrap();
    let store = BundleStore::new(dir.path().into());
    std::fs::create_dir(dir.path().join("pending")).unwrap();
    assert!(store.list().unwrap().is_empty());
    assert!(store.load("pending").is_err());
  }

  #[test]
  fn concurrent_installs_publish_complete_immutable_versions() {
    use std::sync::{Arc, Barrier};
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(BundleStore::new(dir.path().into()));
    let bytes = fixture("test", 1);
    let hash = digest(&bytes);
    let barrier = Arc::new(Barrier::new(8));
    std::thread::scope(|scope| {
      for _ in 0..8 {
        let store = store.clone();
        let barrier = barrier.clone();
        let bytes = &bytes;
        let hash = &hash;
        scope.spawn(move || {
          barrier.wait();
          assert_eq!(store.install(bytes, hash).unwrap().sha256, *hash);
          assert_eq!(store.load("test").unwrap().manifest.name, "Test");
        });
      }
    });
    assert_eq!(
      std::fs::read_dir(dir.path().join("test")).unwrap().count(),
      2
    );
    assert_eq!(store.list().unwrap()[0].sha256, hash);
  }

  #[test]
  fn listing_keeps_manifest_and_hash_from_the_same_version_during_updates() {
    use std::sync::{Arc, Barrier};
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(BundleStore::new(dir.path().into()));
    let first = fixture("test", 1);
    let mut second: Bundle = parse(&first).unwrap();
    second.manifest.name = "Updated".into();
    second.manifest.version = "2".into();
    let second = serde_json::to_vec(&second).unwrap();
    let first_hash = digest(&first);
    let second_hash = digest(&second);
    store.install(&first, &first_hash).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    std::thread::scope(|scope| {
      let writer_store = store.clone();
      let writer_barrier = barrier.clone();
      let first_hash = &first_hash;
      let second_hash = &second_hash;
      scope.spawn(move || {
        writer_barrier.wait();
        for _ in 0..40 {
          writer_store.install(&second, second_hash).unwrap();
          writer_store.install(&first, first_hash).unwrap();
        }
      });
      barrier.wait();
      for _ in 0..200 {
        let installed = store.list().unwrap().remove(0);
        match installed.manifest.name.as_str() {
          "Test" => assert_eq!(installed.sha256, *first_hash),
          "Updated" => assert_eq!(installed.sha256, *second_hash),
          _ => panic!("unexpected manifest"),
        }
      }
    });
  }
}

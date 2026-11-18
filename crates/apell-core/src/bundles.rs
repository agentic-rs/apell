mod source;
mod store;

pub use apell_package::{
  digest, pack_directory, parse, valid_hash, validate_manifest, Bundle, Manifest, API_VERSION,
  MAX_BUNDLE_BYTES, MAX_ENTRIES, MAX_EXPANDED_BYTES,
};
pub use source::{GitHubSource, PackageSource, StoreChannel, StoreSource};
use std::{io::Read, path::Path};
pub use store::{
  AppletListReport, BundleStore, InstalledApplet, StoreSwitchSnapshot, UnavailableApplet,
};

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

pub(crate) fn sync_directory(path: &Path) -> Result<(), String> {
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

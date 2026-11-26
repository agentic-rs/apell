mod source;
mod store;

pub(crate) use apell_package::sync_directory;
pub use apell_package::{
  atomic_write, digest, pack_directory, parse, valid_hash, validate_manifest, Bundle, Manifest,
  API_VERSION, MAX_BUNDLE_BYTES, MAX_ENTRIES, MAX_EXPANDED_BYTES,
};
pub use source::{GitHubSource, PackageSource, StoreChannel, StoreSource};
use std::{
  fs,
  io::{self, Read},
  path::Path,
};
pub use store::{
  AppletListReport, BundleStore, InstalledApplet, StoreSwitchSnapshot, UnavailableApplet,
};

enum ArchiveError {
  Missing,
  Unreadable,
  InvalidType,
  Damaged,
}

impl ArchiveError {
  fn from_read(error: io::Error) -> Self {
    if error.kind() == io::ErrorKind::NotFound {
      Self::Missing
    } else {
      Self::Unreadable
    }
  }

  fn message(&self) -> &'static str {
    match self {
      Self::Missing => "Installed bundle is missing.",
      Self::Unreadable => "Could not read installed bundle.",
      Self::InvalidType => "Installed bundle path is not a regular file.",
      Self::Damaged => "Installed bundle failed integrity verification.",
    }
  }
}

fn read_version(path: &Path, expected_hash: &str) -> Result<Vec<u8>, ArchiveError> {
  let metadata = fs::symlink_metadata(path).map_err(ArchiveError::from_read)?;
  if !metadata.is_file() {
    return Err(ArchiveError::InvalidType);
  }
  let file = fs::File::open(path).map_err(ArchiveError::from_read)?;
  let mut bytes = Vec::new();
  file
    .take(MAX_BUNDLE_BYTES as u64 + 1)
    .read_to_end(&mut bytes)
    .map_err(ArchiveError::from_read)?;
  if bytes.len() > MAX_BUNDLE_BYTES || digest(&bytes) != expected_hash {
    return Err(ArchiveError::Damaged);
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
      Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_version(path, hash)
        .map(|_| ())
        .map_err(|error| error.message().into()),
      Err(_) => Err("Could not publish bundle version.".into()),
    }
  })();
  let _ = std::fs::remove_file(temporary);
  result
}

#[cfg(test)]
mod tests;

use crate::MAX_CREDENTIAL_BYTES;
use sha2::{Digest, Sha256};
use std::{
  fs,
  io::{self, Read, Write},
  os::unix::fs::{DirBuilderExt, PermissionsExt},
  path::{Path, PathBuf},
};

/// Stores only synthetic credentials in the isolated local development app.
pub(super) struct Credentials {
  directory: PathBuf,
}

impl Credentials {
  pub(super) fn new(directory: PathBuf) -> Self {
    Self { directory }
  }

  pub(super) fn read(&self, account: &str) -> Result<Option<String>, String> {
    let path = self.slot(account)?;
    match self.read_file(&path) {
      Ok(value) => Ok(Some(value)),
      Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
      Err(_) => Err("Could not read the local test credential.".into()),
    }
  }

  pub(super) fn write(&self, account: &str, value: Option<&str>) -> Result<(), String> {
    let path = self.slot(account)?;
    match value {
      Some(value) => self
        .replace_file(&path, value)
        .map_err(|_| "Could not save the local test credential.".into()),
      None => match self.check_directory().and_then(|()| fs::remove_file(&path)) {
        Ok(()) => fs::File::open(&self.directory)
          .and_then(|dir| dir.sync_all())
          .map_err(|_| "Could not persist removal of the local test credential.".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("Could not remove the local test credential.".into()),
      },
    }
  }

  fn slot(&self, account: &str) -> Result<PathBuf, String> {
    if account.is_empty() {
      return Err("Invalid credential slot.".into());
    }
    Ok(
      self
        .directory
        .join(format!("{:x}", Sha256::digest(account))),
    )
  }

  fn check_directory(&self) -> io::Result<()> {
    let metadata = fs::symlink_metadata(&self.directory)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
      return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
  }

  fn read_file(&self, path: &Path) -> io::Result<String> {
    self.check_directory()?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
      return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut value = String::new();
    fs::File::open(path)?
      .take((MAX_CREDENTIAL_BYTES + 1) as u64)
      .read_to_string(&mut value)?;
    if value.len() > MAX_CREDENTIAL_BYTES {
      return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(value)
  }

  fn replace_file(&self, path: &Path, value: &str) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(&self.directory) {
      Ok(()) => {}
      Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
      Err(error) => return Err(error),
    }
    self.check_directory()?;
    let mut file = tempfile::Builder::new()
      .permissions(fs::Permissions::from_mode(0o600))
      .tempfile_in(&self.directory)?;
    file.write_all(value.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    fs::File::open(&self.directory)?.sync_all()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{os::unix::fs::symlink, thread};

  #[test]
  fn credentials_persist_between_instances_and_files_are_private() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("mock-credentials");
    let store = Credentials::new(directory.clone());
    let account = "../fixture/private-slot";
    assert_eq!(store.read(account).unwrap(), None);
    store.write(account, None).unwrap();
    assert!(!directory.exists());
    store.write(account, Some("synthetic-key")).unwrap();
    let path = store.slot(account).unwrap();
    assert_eq!(path.parent(), Some(directory.as_path()));
    assert_eq!(path.file_name().unwrap().len(), 64);
    assert_eq!(
      fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
      0o700
    );
    assert_eq!(
      fs::metadata(&path).unwrap().permissions().mode() & 0o777,
      0o600
    );
    drop(store);

    let store = Credentials::new(directory);
    assert_eq!(
      store.read(account).unwrap().as_deref(),
      Some("synthetic-key")
    );
    store.write(account, Some("updated-key")).unwrap();
    assert_eq!(store.read(account).unwrap().as_deref(), Some("updated-key"));
    store.write(account, Some("")).unwrap();
    assert_eq!(store.read(account).unwrap().as_deref(), Some(""));
    store.write(account, None).unwrap();
    store.write(account, None).unwrap();
    assert_eq!(store.read(account).unwrap(), None);
    assert!(store.read("").is_err());
  }

  #[test]
  fn concurrent_writes_to_independent_slots_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("mock-credentials");
    thread::scope(|scope| {
      for (slot, value) in [("first", "synthetic-first"), ("second", "synthetic-second")] {
        let directory = directory.clone();
        scope.spawn(move || {
          Credentials::new(directory)
            .write(slot, Some(value))
            .unwrap()
        });
      }
    });
    let store = Credentials::new(directory);
    assert_eq!(
      store.read("first").unwrap().as_deref(),
      Some("synthetic-first")
    );
    assert_eq!(
      store.read("second").unwrap().as_deref(),
      Some("synthetic-second")
    );
  }

  #[test]
  fn unsafe_or_oversized_files_are_not_read() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("mock-credentials");
    let other = root.path().join("outside-store");
    fs::write(&other, "synthetic-outside").unwrap();
    let store = Credentials::new(directory.clone());
    let path = store.slot("fixture").unwrap();
    store.write("fixture", Some("synthetic-key")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.read("fixture").is_err());
    fs::remove_file(&path).unwrap();
    symlink(&other, &path).unwrap();
    assert!(store.read("fixture").is_err());
    store.write("fixture", Some("replacement")).unwrap();
    assert_eq!(fs::read_to_string(&other).unwrap(), "synthetic-outside");
    fs::write(&path, vec![b'x'; MAX_CREDENTIAL_BYTES + 1]).unwrap();
    assert!(store.read("fixture").is_err());
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(store.read("fixture").is_err());
    assert!(store.write("fixture", Some("synthetic-key")).is_err());
  }
}

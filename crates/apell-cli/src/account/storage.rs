use super::{now, store_root, valid_user, User};
use serde::{Deserialize, Serialize};
use std::{
  fs,
  io::{Read, Write},
  path::{Path, PathBuf},
};
use url::Url;

const STORAGE_ERROR: &str = "Could not access private CLI credentials. Check the CLI config directory permissions (directory 0700, files 0600) and unlock Keychain if needed.";

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SavedCredential {
  pub token: String,
  pub expires_at: u64,
  pub user: User,
}

impl SavedCredential {
  pub fn validate(&self) -> Result<(), String> {
    if !valid_token(&self.token) || !valid_user(&self.user) {
      return Err("Invalid saved account credential. Run apell login again.".into());
    }
    if self.expires_at <= now() {
      return Err("The account login has expired. Run apell login again.".into());
    }
    Ok(())
  }
}

pub(super) fn valid_token(value: &str) -> bool {
  value.strip_prefix("apel_user_").is_some_and(|suffix| {
    suffix.len() == 64
      && suffix
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
  })
}

#[derive(Serialize, Deserialize)]
struct Record {
  store: String,
  expires_at: u64,
  user: User,
  backend: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  token: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Config {
  default_store: String,
}

pub(crate) struct CredentialStore {
  root: PathBuf,
  keychain: bool,
}

impl CredentialStore {
  pub fn open() -> Result<Self, String> {
    if !cfg!(unix) {
      return Err("Account credential storage is unsupported on this platform. Pack and GitHub Actions publishing remain available.".into());
    }
    let override_dir = std::env::var_os("APELL_CLI_CONFIG_DIR");
    let keychain = cfg!(target_os = "macos") && override_dir.is_none();
    let root = match override_dir {
      Some(value) if !value.is_empty() => PathBuf::from(value),
      Some(_) => return Err("APELL_CLI_CONFIG_DIR must be an absolute private directory.".into()),
      None => directories::ProjectDirs::from("dev", "apell", "apell")
        .ok_or("Could not locate the CLI config directory.")?
        .config_dir()
        .join("cli"),
    };
    Self::at(root, keychain)
  }

  pub(super) fn at(root: PathBuf, keychain: bool) -> Result<Self, String> {
    if !root.is_absolute() {
      return Err("APELL_CLI_CONFIG_DIR must be an absolute private directory.".into());
    }
    private_directory(&root)?;
    Ok(Self { root, keychain })
  }

  fn path(&self, store: &Url) -> PathBuf {
    self.root.join(format!(
      "{}.json",
      apell_package::digest(store.as_str().as_bytes())
    ))
  }

  pub fn default_store(&self) -> Result<Option<Url>, String> {
    let Some(bytes) = private_read(&self.root.join("config.json"))? else {
      return Ok(None);
    };
    let config: Config = serde_json::from_slice(&bytes).map_err(|_| STORAGE_ERROR)?;
    Ok(Some(store_root(&config.default_store)?))
  }

  pub fn contains(&self, store: &Url) -> Result<bool, String> {
    Ok(self.record(store)?.is_some())
  }

  fn record(&self, store: &Url) -> Result<Option<Record>, String> {
    let Some(bytes) = private_read(&self.path(store))? else {
      return Ok(None);
    };
    let record: Record = serde_json::from_slice(&bytes).map_err(|_| STORAGE_ERROR)?;
    if record.store != store.as_str()
      || record.backend != if self.keychain { "keychain" } else { "file" }
      || self.keychain && record.token.is_some()
    {
      return Err("Saved credential origin or storage backend does not match this Store.".into());
    }
    Ok(Some(record))
  }

  pub fn load(&self, store: &Url) -> Result<SavedCredential, String> {
    let record = self
      .record(store)?
      .ok_or("Not signed in to this Store. Run apell login --store URL.")?;
    let token = if self.keychain {
      keychain_get(store)?
    } else {
      record.token.ok_or(STORAGE_ERROR)?
    };
    if !valid_token(&token) || !valid_user(&record.user) {
      return Err(STORAGE_ERROR.into());
    }
    Ok(SavedCredential {
      token,
      expires_at: record.expires_at,
      user: record.user,
    })
  }

  pub fn save(&self, store: &Url, credential: &SavedCredential) -> Result<(), String> {
    credential.validate()?;
    let record = Record {
      store: store.to_string(),
      expires_at: credential.expires_at,
      user: credential.user.clone(),
      backend: if self.keychain { "keychain" } else { "file" }.into(),
      token: (!self.keychain).then(|| credential.token.clone()),
    };
    if self.keychain {
      keychain_set(store, &credential.token)?;
    }
    private_write(&self.root, &self.path(store), &record)?;
    private_write(
      &self.root,
      &self.root.join("config.json"),
      &Config {
        default_store: store.to_string(),
      },
    )
  }

  pub fn remove(&self, store: &Url) -> Result<(), String> {
    if self.record(store)?.is_none() {
      return Ok(());
    }
    if self.keychain {
      keychain_remove(store)?;
    }
    fs::remove_file(self.path(store)).map_err(|_| STORAGE_ERROR.into())
  }
}

#[cfg(unix)]
fn private_directory(path: &Path) -> Result<(), String> {
  use std::os::unix::fs::{DirBuilderExt, MetadataExt};
  fs::DirBuilder::new()
    .recursive(true)
    .mode(0o700)
    .create(path)
    .map_err(|_| STORAGE_ERROR)?;
  let mut options = fs::OpenOptions::new();
  use std::os::unix::fs::OpenOptionsExt;
  options
    .read(true)
    .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
  let directory = options.open(path).map_err(|_| STORAGE_ERROR)?;
  let metadata = directory.metadata().map_err(|_| STORAGE_ERROR)?;
  // Check ownership and mode on the opened inode, not only the path.
  if !metadata.is_dir()
    || metadata.uid() != unsafe { libc::geteuid() }
    || metadata.mode() & 0o077 != 0
  {
    return Err(STORAGE_ERROR.into());
  }
  Ok(())
}

#[cfg(not(unix))]
fn private_directory(_: &Path) -> Result<(), String> {
  Err(STORAGE_ERROR.into())
}

fn private_read(path: &Path) -> Result<Option<Vec<u8>>, String> {
  let mut options = fs::OpenOptions::new();
  options.read(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
  }
  let file = match options.open(path) {
    Ok(file) => file,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
    Err(_) => return Err(STORAGE_ERROR.into()),
  };
  let metadata = file.metadata().map_err(|_| STORAGE_ERROR)?;
  #[cfg(unix)]
  {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
      return Err(STORAGE_ERROR.into());
    }
  }
  if !metadata.is_file() || metadata.len() > 16_384 {
    return Err(STORAGE_ERROR.into());
  }
  let mut bytes = Vec::new();
  file
    .take(16_385)
    .read_to_end(&mut bytes)
    .map_err(|_| STORAGE_ERROR)?;
  if bytes.len() > 16_384 {
    return Err(STORAGE_ERROR.into());
  }
  Ok(Some(bytes))
}

fn private_write(root: &Path, path: &Path, value: &impl Serialize) -> Result<(), String> {
  private_directory(root)?;
  // Reject existing links and unsafe files before atomically replacing the entry.
  private_read(path)?;
  let bytes = serde_json::to_vec(value).map_err(|_| STORAGE_ERROR)?;
  let mut temporary = tempfile::NamedTempFile::new_in(root).map_err(|_| STORAGE_ERROR)?;
  temporary.write_all(&bytes).map_err(|_| STORAGE_ERROR)?;
  temporary.as_file().sync_all().map_err(|_| STORAGE_ERROR)?;
  temporary.persist(path).map_err(|_| STORAGE_ERROR)?;
  Ok(())
}

#[cfg(target_os = "macos")]
fn keychain_get(store: &Url) -> Result<String, String> {
  let bytes = security_framework::passwords::get_generic_password("dev.apell.cli", store.as_str())
    .map_err(|_| STORAGE_ERROR)?;
  String::from_utf8(bytes).map_err(|_| STORAGE_ERROR.into())
}
#[cfg(target_os = "macos")]
fn keychain_set(store: &Url, token: &str) -> Result<(), String> {
  security_framework::passwords::set_generic_password(
    "dev.apell.cli",
    store.as_str(),
    token.as_bytes(),
  )
  .map_err(|_| STORAGE_ERROR.into())
}
#[cfg(target_os = "macos")]
fn keychain_remove(store: &Url) -> Result<(), String> {
  match security_framework::passwords::delete_generic_password("dev.apell.cli", store.as_str()) {
    Ok(()) => Ok(()),
    Err(error) if error.code() == -25300 => Ok(()),
    Err(_) => Err(STORAGE_ERROR.into()),
  }
}
#[cfg(not(target_os = "macos"))]
fn keychain_get(_: &Url) -> Result<String, String> {
  Err(STORAGE_ERROR.into())
}
#[cfg(not(target_os = "macos"))]
fn keychain_set(_: &Url, _: &str) -> Result<(), String> {
  Err(STORAGE_ERROR.into())
}
#[cfg(not(target_os = "macos"))]
fn keychain_remove(_: &Url) -> Result<(), String> {
  Err(STORAGE_ERROR.into())
}

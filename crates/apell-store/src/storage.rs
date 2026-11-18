use crate::{
  error::{Result, StoreError},
  model::*,
  packages::{self, PublishAuthority},
};
use apell_package::{digest, parse, valid_hash, valid_name, MAX_BUNDLE_BYTES};
use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use semver::Version;
use sha2::{Digest, Sha256};
use std::{
  fs::{self, File},
  io::{Read, Seek, SeekFrom, Write},
  path::{Path, PathBuf},
  time::Duration,
};

const MAX_APPLETS: i64 = 256;
const MAX_RELEASES: i64 = 1024;

#[derive(Clone)]
pub struct Store {
  root: PathBuf,
}

struct StoredRelease {
  applet_id: String,
  name: String,
  metadata: PublishMetadata,
  icon_sha256: Option<String>,
  release: Release,
}

impl Store {
  pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
    let store = Self { root: root.into() };
    fs::create_dir_all(store.root.join("packages")).map_err(|_| StoreError::internal())?;
    fs::create_dir_all(store.root.join("icons")).map_err(|_| StoreError::internal())?;
    let connection = store.connection()?;
    connection
      .execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
      CREATE TABLE IF NOT EXISTS applets (
        applet_id TEXT PRIMARY KEY,
        latest_version TEXT NOT NULL,
        stable_version TEXT
      );
      CREATE TABLE IF NOT EXISTS releases (
        applet_id TEXT NOT NULL,
        version TEXT NOT NULL,
        name TEXT NOT NULL,
        metadata TEXT NOT NULL,
        icon_sha256 TEXT,
        release_json TEXT NOT NULL,
        PRIMARY KEY (applet_id, version)
      );
      CREATE INDEX IF NOT EXISTS release_icons ON releases(icon_sha256);",
      )
      .map_err(|_| StoreError::internal())?;
    crate::auth::migrate(&connection)?;
    packages::migrate(&connection)?;
    Ok(store)
  }

  pub(crate) fn connection(&self) -> Result<Connection> {
    let connection =
      Connection::open(self.root.join("catalog.sqlite3")).map_err(|_| StoreError::internal())?;
    connection
      .busy_timeout(Duration::from_secs(5))
      .map_err(|_| StoreError::internal())?;
    connection
      .pragma_update(None, "foreign_keys", true)
      .map_err(|_| StoreError::internal())?;
    Ok(connection)
  }

  pub fn publish(
    &self,
    bytes: Vec<u8>,
    metadata: PublishMetadata,
    icon: Option<Vec<u8>>,
  ) -> Result<PublishResult> {
    self.publish_authorized(bytes, metadata, icon, PublishAuthority::Administrator)
  }

  pub fn publish_authorized(
    &self,
    bytes: Vec<u8>,
    metadata: PublishMetadata,
    icon: Option<Vec<u8>>,
    authority: PublishAuthority,
  ) -> Result<PublishResult> {
    metadata.validate()?;
    let bundle = parse(&bytes).map_err(|message| {
      StoreError::new(
        axum::http::StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_package",
        message,
      )
    })?;
    let manifest = bundle.manifest;
    drop(bundle.files);
    let version = Version::parse(&manifest.version)
      .map_err(|_| StoreError::invalid("Invalid package version."))?;
    let icon_sha256 = icon
      .as_ref()
      .map(|bytes| validate_icon(bytes).map(|_| digest(bytes)))
      .transpose()?;
    let hash = digest(&bytes);
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction_with_behavior(TransactionBehavior::Immediate)
      .map_err(|_| StoreError::internal())?;
    packages::authorize_transaction(&transaction, &manifest.applet_id, &authority)?;
    if let Some(existing) = read_release(&transaction, &manifest.applet_id, &manifest.version)? {
      if existing.release.sha256 != hash {
        return Err(StoreError::conflict(
          "This applet version already contains different bytes. Publish a new version.",
        ));
      }
      if existing.metadata != metadata || existing.icon_sha256 != icon_sha256 {
        return Err(StoreError::conflict(
          "This applet version has different listing metadata or icon. Published releases are immutable; publish a new version.",
        ));
      }
      self.verified_file("packages", &hash, "apel", existing.release.size_bytes)?;
      return Ok(PublishResult {
        applet_id: manifest.applet_id,
        version: manifest.version,
        sha256: hash,
        created: false,
      });
    }
    let versions = release_versions(&transaction, &manifest.applet_id)?;
    if versions.len() as i64 >= MAX_RELEASES {
      return Err(StoreError::conflict(
        "This applet has reached the store's release limit.",
      ));
    }
    for previous in &versions {
      let previous = Version::parse(previous).map_err(|_| StoreError::internal())?;
      if previous.cmp_precedence(&version).is_eq() {
        return Err(StoreError::conflict("A version with the same semantic precedence already exists. Advance the version instead of changing build metadata."));
      }
    }
    let previous_latest: Option<(String, Option<String>)> = transaction
      .query_row(
        "SELECT latest_version, stable_version FROM applets WHERE applet_id = ?1",
        [&manifest.applet_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
      )
      .optional()
      .map_err(|_| StoreError::internal())?;
    if previous_latest.is_none() {
      let count: i64 = transaction
        .query_row("SELECT COUNT(*) FROM applets", [], |row| row.get(0))
        .map_err(|_| StoreError::internal())?;
      if count >= MAX_APPLETS {
        return Err(StoreError::conflict(
          "The store has reached its applet limit.",
        ));
      }
    }
    self.write_content("packages", &hash, "apel", &bytes)?;
    if let Some(icon) = &icon {
      self.write_content(
        "icons",
        icon_sha256.as_deref().ok_or_else(StoreError::internal)?,
        "png",
        icon,
      )?;
    }
    let release = Release {
      version: manifest.version.clone(),
      sha256: hash.clone(),
      size_bytes: bytes.len() as u64,
      published_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
      release_notes: metadata.release_notes.clone(),
      api_version: manifest.api_version,
      permissions: manifest.permissions,
      network: manifest.network,
      has_backend: manifest.lib.is_some(),
      download_url: format!(
        "/api/v1/applets/{}/versions/{}/download",
        manifest.applet_id, manifest.version
      ),
      prerelease: !version.pre.is_empty(),
    };
    transaction.execute("INSERT INTO releases (applet_id,version,name,metadata,icon_sha256,release_json) VALUES (?1,?2,?3,?4,?5,?6)", params![
      manifest.applet_id, manifest.version, manifest.name,
      serde_json::to_string(&metadata).map_err(|_| StoreError::internal())?, icon_sha256,
      serde_json::to_string(&release).map_err(|_| StoreError::internal())?,
    ]).map_err(|_| StoreError::internal())?;
    let (latest, stable) = match previous_latest {
      Some((latest, stable)) => {
        let prior = Version::parse(&latest).map_err(|_| StoreError::internal())?;
        let latest = if version.cmp_precedence(&prior).is_gt() {
          manifest.version.clone()
        } else {
          latest
        };
        let stable = if version.pre.is_empty()
          && stable.as_ref().is_none_or(|old| {
            Version::parse(old).is_ok_and(|old| version.cmp_precedence(&old).is_gt())
          }) {
          Some(manifest.version.clone())
        } else {
          stable
        };
        (latest, stable)
      }
      None => (
        manifest.version.clone(),
        version.pre.is_empty().then(|| manifest.version.clone()),
      ),
    };
    transaction.execute("INSERT INTO applets (applet_id,latest_version,stable_version) VALUES (?1,?2,?3)
      ON CONFLICT(applet_id) DO UPDATE SET latest_version=excluded.latest_version,stable_version=excluded.stable_version",
      params![manifest.applet_id, latest, stable]).map_err(|_| StoreError::internal())?;
    packages::audit_publication(&transaction, &manifest.applet_id, &authority)?;
    transaction.commit().map_err(|_| StoreError::internal())?;
    Ok(PublishResult {
      applet_id: manifest.applet_id,
      version: manifest.version,
      sha256: hash,
      created: true,
    })
  }

  pub fn catalog(&self) -> Result<Catalog> {
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction()
      .map_err(|_| StoreError::internal())?;
    let mut statement = transaction
      .prepare("SELECT applet_id FROM applets ORDER BY applet_id LIMIT ?1")
      .map_err(|_| StoreError::internal())?;
    let ids = statement
      .query_map([MAX_APPLETS], |row| row.get::<_, String>(0))
      .map_err(|_| StoreError::internal())?
      .collect::<std::result::Result<Vec<_>, _>>()
      .map_err(|_| StoreError::internal())?;
    let mut applets = ids
      .into_iter()
      .map(|id| applet(&transaction, &id))
      .collect::<Result<Vec<_>>>()?;
    applets.sort_by(|left, right| {
      left
        .name
        .to_lowercase()
        .cmp(&right.name.to_lowercase())
        .then(left.applet_id.cmp(&right.applet_id))
    });
    Ok(Catalog { applets })
  }

  pub fn detail(&self, id: &str) -> Result<AppletDetail> {
    if !valid_name(id) {
      return Err(StoreError::missing());
    }
    let mut connection = self.connection()?;
    let transaction = connection
      .transaction()
      .map_err(|_| StoreError::internal())?;
    let applet = applet(&transaction, id)?;
    let mut statement = transaction
      .prepare("SELECT release_json FROM releases WHERE applet_id=?1 LIMIT ?2")
      .map_err(|_| StoreError::internal())?;
    let mut releases = statement
      .query_map(params![id, MAX_RELEASES], |row| row.get::<_, String>(0))
      .map_err(|_| StoreError::internal())?
      .map(|value| {
        serde_json::from_str::<Release>(&value.map_err(|_| StoreError::internal())?)
          .map_err(|_| StoreError::internal())
      })
      .collect::<Result<Vec<_>>>()?;
    releases.sort_by(|left, right| {
      // Versions were validated before storage; a corrupt row is rejected above/on download.
      Version::parse(&right.version)
        .ok()
        .cmp(&Version::parse(&left.version).ok())
    });
    Ok(AppletDetail { applet, releases })
  }

  pub fn download(&self, id: &str, version: &str) -> Result<(File, Release)> {
    if !valid_name(id) || Version::parse(version).is_err() {
      return Err(StoreError::missing());
    }
    let record = read_release(&self.connection()?, id, version)?.ok_or_else(StoreError::missing)?;
    let file = self.verified_file(
      "packages",
      &record.release.sha256,
      "apel",
      record.release.size_bytes,
    )?;
    Ok((file, record.release))
  }

  pub fn icon(&self, hash: &str) -> Result<(File, u64)> {
    if !valid_hash(hash) || hash != hash.to_ascii_lowercase() {
      return Err(StoreError::missing());
    }
    let exists: bool = self
      .connection()?
      .query_row(
        "SELECT EXISTS(SELECT 1 FROM releases WHERE icon_sha256=?1)",
        [hash],
        |row| row.get(0),
      )
      .map_err(|_| StoreError::internal())?;
    if !exists {
      return Err(StoreError::missing());
    }
    let file_path = self.content_path("icons", hash, "png");
    let size = fs::metadata(file_path)
      .map_err(|_| StoreError::internal())?
      .len();
    if size > MAX_ICON_BYTES as u64 {
      return Err(StoreError::internal());
    }
    Ok((self.verified_file("icons", hash, "png", size)?, size))
  }

  fn content_path(&self, directory: &str, hash: &str, extension: &str) -> PathBuf {
    self
      .root
      .join(directory)
      .join(format!("{hash}.{extension}"))
  }

  fn write_content(
    &self,
    directory: &str,
    hash: &str,
    extension: &str,
    bytes: &[u8],
  ) -> Result<()> {
    let parent = self.root.join(directory);
    let destination = self.content_path(directory, hash, extension);
    let mut temporary =
      tempfile::NamedTempFile::new_in(&parent).map_err(|_| StoreError::internal())?;
    temporary
      .write_all(bytes)
      .and_then(|_| temporary.as_file().sync_all())
      .map_err(|_| StoreError::internal())?;
    match temporary.persist_noclobber(destination) {
      Ok(_) => sync_directory(&parent),
      Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => self
        .verified_file(directory, hash, extension, bytes.len() as u64)
        .map(|_| ()),
      Err(_) => Err(StoreError::internal()),
    }
  }

  fn verified_file(&self, directory: &str, hash: &str, extension: &str, size: u64) -> Result<File> {
    if !valid_hash(hash) || size > MAX_BUNDLE_BYTES as u64 {
      return Err(StoreError::internal());
    }
    let mut file = File::open(self.content_path(directory, hash, extension))
      .map_err(|_| StoreError::internal())?;
    if file.metadata().map_err(|_| StoreError::internal())?.len() != size {
      return Err(StoreError::internal());
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut remaining = size;
    while remaining > 0 {
      let limit = buffer.len().min(remaining as usize);
      let count = file
        .read(&mut buffer[..limit])
        .map_err(|_| StoreError::internal())?;
      if count == 0 {
        return Err(StoreError::internal());
      }
      hasher.update(&buffer[..count]);
      remaining -= count as u64;
    }
    if format!("{:x}", hasher.finalize()) != hash {
      return Err(StoreError::internal());
    }
    file
      .seek(SeekFrom::Start(0))
      .map_err(|_| StoreError::internal())?;
    Ok(file)
  }
}

fn sync_directory(path: &Path) -> Result<()> {
  #[cfg(unix)]
  File::open(path)
    .and_then(|file| file.sync_all())
    .map_err(|_| StoreError::internal())?;
  #[cfg(not(unix))]
  let _ = path;
  Ok(())
}

fn read_release(connection: &Connection, id: &str, version: &str) -> Result<Option<StoredRelease>> {
  let row = connection.query_row("SELECT name,metadata,icon_sha256,release_json FROM releases WHERE applet_id=?1 AND version=?2", params![id, version], |row| {
    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, String>(3)?))
  }).optional().map_err(|_| StoreError::internal())?;
  row
    .map(|(name, metadata, icon_sha256, release)| {
      Ok(StoredRelease {
        applet_id: id.into(),
        name,
        metadata: serde_json::from_str(&metadata).map_err(|_| StoreError::internal())?,
        icon_sha256,
        release: serde_json::from_str(&release).map_err(|_| StoreError::internal())?,
      })
    })
    .transpose()
}

fn release_versions(connection: &Connection, id: &str) -> Result<Vec<String>> {
  let mut statement = connection
    .prepare("SELECT version FROM releases WHERE applet_id=?1 LIMIT ?2")
    .map_err(|_| StoreError::internal())?;
  let versions = statement
    .query_map(params![id, MAX_RELEASES], |row| row.get(0))
    .map_err(|_| StoreError::internal())?
    .collect::<std::result::Result<Vec<_>, _>>()
    .map_err(|_| StoreError::internal())?;
  Ok(versions)
}

fn applet(connection: &Connection, id: &str) -> Result<Applet> {
  let (latest, stable): (String, Option<String>) = connection
    .query_row(
      "SELECT latest_version,stable_version FROM applets WHERE applet_id=?1",
      [id],
      |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(|_| StoreError::internal())?
    .ok_or_else(StoreError::missing)?;
  let record = read_release(connection, id, &latest)?.ok_or_else(StoreError::internal)?;
  let latest_stable_release = stable
    .map(|version| {
      read_release(connection, id, &version)?
        .map(|record| record.release)
        .ok_or_else(StoreError::internal)
    })
    .transpose()?;
  let latest_nightly_release = if record.release.prerelease {
    Some(record.release.clone())
  } else {
    // Derive the Nightly head from existing immutable versions. No schema
    // change or backfill is needed, and older Store binaries can still read
    // the same database. Only load metadata for the winning prerelease.
    let mut nightly: Option<Version> = None;
    for value in release_versions(connection, id)? {
      let version = Version::parse(&value).map_err(|_| StoreError::internal())?;
      if !version.pre.is_empty()
        && nightly
          .as_ref()
          .is_none_or(|previous| version.cmp_precedence(previous).is_gt())
      {
        nightly = Some(version);
      }
    }
    nightly
      .map(|version| {
        read_release(connection, id, &version.to_string())?
          .map(|record| record.release)
          .ok_or_else(StoreError::internal)
      })
      .transpose()?
  };
  Ok(Applet {
    applet_id: record.applet_id,
    name: record.name,
    description: record.metadata.description,
    project_url: record.metadata.project_url,
    icon_url: record.icon_sha256.map(|hash| format!("/media/{hash}.png")),
    latest_release: record.release,
    latest_stable_release,
    latest_nightly_release,
  })
}

fn validate_icon(bytes: &[u8]) -> Result<()> {
  if bytes.is_empty() || bytes.len() > MAX_ICON_BYTES {
    return Err(StoreError::invalid(
      "Icon must be a PNG of at most 256 KiB.",
    ));
  }
  let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
  decoder.set_limits(png::Limits {
    bytes: 8 * 1024 * 1024,
  });
  let mut reader = decoder
    .read_info()
    .map_err(|_| StoreError::invalid("Icon is not a valid PNG."))?;
  let info = reader.info();
  if info.width == 0
    || info.height == 0
    || info.width > 1024
    || info.height > 1024
    || info.animation_control.is_some()
  {
    return Err(StoreError::invalid(
      "Icon must be a still PNG no larger than 1024 × 1024.",
    ));
  }
  let size = reader.output_buffer_size();
  if size > 8 * 1024 * 1024 {
    return Err(StoreError::invalid("Icon decoded content is too large."));
  }
  reader
    .next_frame(&mut vec![0; size])
    .map_err(|_| StoreError::invalid("Icon is not a valid PNG."))?;
  reader
    .finish()
    .map_err(|_| StoreError::invalid("Icon is not a valid PNG."))?;
  Ok(())
}

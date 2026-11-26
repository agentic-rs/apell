use super::super::{
  model::{lowercase_hash, release_download_path, valid_applet_name},
  StoreRelease,
};
use crate::bundles::{atomic_write, digest, StoreChannel, StoreSource, MAX_BUNDLE_BYTES};
use serde::{Deserialize, Serialize};
use std::{
  collections::BTreeSet,
  fs::Metadata,
  io::ErrorKind,
  path::{Path, PathBuf},
};
use tokio::io::AsyncReadExt;

const LEGACY_METADATA_FILE: &str = "pending.json";
const MAX_METADATA_BYTES: u64 = 4_096;
const MAX_PENDING_DOWNLOADS: usize = 4;
pub(super) const STORAGE_ERROR: &str = "Could not access the Store download cache.";

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadIdentity {
  origin: String,
  applet_id: String,
  version: String,
  channel: StoreChannel,
  sha256: String,
}

impl DownloadIdentity {
  fn checkpoint(&self, cache: &Path) -> DownloadCheckpoint {
    DownloadCheckpoint::for_release(
      cache,
      &self.origin,
      &release_download_path(&self.applet_id, &self.version),
      &self.sha256,
    )
  }

  fn valid(&self) -> bool {
    let source = StoreSource {
      store_url: self.origin.clone(),
      applet_id: self.applet_id.clone(),
      channel: self.channel,
    };
    source.validate().is_ok()
      && lowercase_hash(&self.sha256)
      && self.version.len() <= 80
      && semver::Version::parse(&self.version)
        .is_ok_and(|version| StoreChannel::from_prerelease(!version.pre.is_empty()) == self.channel)
  }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DownloadMetadata {
  identity: DownloadIdentity,
  name: String,
  size_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct PendingDownload {
  #[serde(flatten)]
  identity: DownloadIdentity,
  name: String,
  size_bytes: u64,
  downloaded_bytes: u64,
}

async fn read_metadata(path: &Path) -> Result<Option<DownloadMetadata>, String> {
  match tokio::fs::symlink_metadata(path).await {
    Ok(metadata) if !metadata.is_file() || metadata.len() > MAX_METADATA_BYTES => return Ok(None),
    Ok(_) => (),
    Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
    Err(_) => return Err(STORAGE_ERROR.into()),
  }
  let file = match tokio::fs::File::open(path).await {
    Ok(file) => file,
    Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
    Err(_) => return Err(STORAGE_ERROR.into()),
  };
  let mut bytes = Vec::new();
  file
    .take(MAX_METADATA_BYTES + 1)
    .read_to_end(&mut bytes)
    .await
    .map_err(|_| STORAGE_ERROR)?;
  if bytes.len() as u64 > MAX_METADATA_BYTES {
    return Ok(None);
  }
  Ok(
    serde_json::from_slice::<DownloadMetadata>(&bytes)
      .ok()
      .filter(|metadata| {
        metadata.identity.valid()
          && valid_applet_name(&metadata.name)
          && (1..=MAX_BUNDLE_BYTES as u64).contains(&metadata.size_bytes)
      }),
  )
}

async fn remove_if_present(path: &Path) -> Result<(), String> {
  match tokio::fs::remove_file(path).await {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
    Err(_) => Err(STORAGE_ERROR.into()),
  }
}

async fn write_metadata(path: PathBuf, metadata: &DownloadMetadata) -> Result<(), String> {
  let bytes = serde_json::to_vec(metadata).map_err(|_| STORAGE_ERROR)?;
  tokio::task::spawn_blocking(move || atomic_write(&path, &bytes))
    .await
    .map_err(|_| STORAGE_ERROR)?
    .map_err(|_| STORAGE_ERROR.into())
}

/// A checkpoint binds saved bytes to the exact origin, route and reviewed hash.
pub struct DownloadCheckpoint(pub(super) PathBuf);

impl DownloadCheckpoint {
  pub fn new(cache: &Path, origin: &str, release: &StoreRelease) -> Self {
    Self::for_release(cache, origin, &release.download_url, &release.sha256)
  }

  fn for_release(cache: &Path, origin: &str, download_path: &str, sha256: &str) -> Self {
    let identity = digest(format!("{origin}\n{download_path}\n{sha256}").as_bytes());
    Self(cache.join(format!("{identity}.part")))
  }

  fn metadata_path(&self) -> PathBuf {
    self.0.with_extension("json")
  }

  /// Call under the same gate used to start, resume, and discard downloads.
  pub async fn pending_downloads(cache: &Path) -> Result<Vec<PendingDownload>, String> {
    saved_downloads(cache).await
  }

  /// Delete only the requested release's retained bytes and metadata.
  pub async fn discard(cache: &Path, identity: &DownloadIdentity) -> Result<bool, String> {
    if !identity.valid() || !cache_present(cache).await? {
      return Ok(false);
    }
    migrate_legacy(cache).await?;
    let checkpoint = identity.checkpoint(cache);
    if !checkpoint
      .pending_entry(cache)
      .await?
      .is_some_and(|(pending, _)| &pending.identity == identity)
    {
      return Ok(false);
    }
    checkpoint.finish().await?;
    Ok(true)
  }

  async fn part_metadata(&self, size: u64) -> Result<Option<Metadata>, String> {
    match tokio::fs::symlink_metadata(&self.0).await {
      Ok(metadata) if metadata.is_file() && metadata.len() <= size => Ok(Some(metadata)),
      Ok(_) => Ok(None),
      Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
      Err(_) => Err(STORAGE_ERROR.into()),
    }
  }

  pub(crate) async fn received(&self, size: u64) -> u64 {
    self
      .part_metadata(size)
      .await
      .ok()
      .flatten()
      .map_or(0, |metadata| metadata.len())
  }

  /// Check recorded progress for the reviewed size without migrating or pruning.
  pub async fn reviewed_received(&self, size: u64) -> u64 {
    let Some(cache) = self.0.parent() else {
      return 0;
    };
    if let Ok(Some((pending, _))) = self.pending_entry(cache).await {
      return if pending.size_bytes == size {
        pending.downloaded_bytes
      } else {
        0
      };
    }
    if read_metadata(&cache.join(LEGACY_METADATA_FILE))
      .await
      .ok()
      .flatten()
      .is_some_and(|legacy| {
        legacy.size_bytes == size && legacy.identity.checkpoint(cache).0 == self.0
      })
    {
      return self.received(size).await;
    }
    0
  }

  async fn pending_entry(
    &self,
    cache: &Path,
  ) -> Result<Option<(PendingDownload, Metadata)>, String> {
    let Some(metadata) = read_metadata(&self.metadata_path()).await? else {
      return Ok(None);
    };
    if metadata.identity.checkpoint(cache).0 != self.0 {
      return Ok(None);
    }
    let Some(part) = self
      .part_metadata(metadata.size_bytes)
      .await?
      .filter(|part| part.len() > 0)
    else {
      return Ok(None);
    };
    Ok(Some((
      PendingDownload {
        identity: metadata.identity,
        name: metadata.name,
        size_bytes: metadata.size_bytes,
        downloaded_bytes: part.len(),
      },
      part,
    )))
  }

  #[cfg(test)]
  pub(crate) async fn prepare_for_test(&self, bytes: &[u8]) {
    assert!(self.metadata_path().is_file());
    tokio::fs::write(&self.0, bytes).await.unwrap();
  }

  pub(super) async fn prepare(
    &self,
    origin: &str,
    applet_id: &str,
    name: String,
    release: &StoreRelease,
  ) -> Result<(), String> {
    let directory = self.0.parent().ok_or(STORAGE_ERROR)?;
    let metadata = DownloadMetadata {
      identity: DownloadIdentity {
        origin: origin.to_owned(),
        applet_id: applet_id.to_owned(),
        version: release.version.clone(),
        channel: StoreChannel::from_prerelease(release.prerelease),
        sha256: release.sha256.clone(),
      },
      name,
      size_bytes: release.size_bytes,
    };
    if metadata.identity.checkpoint(directory).0 != self.0 {
      return Err(STORAGE_ERROR.into());
    }
    let saved = saved_downloads(directory).await?;
    if let Some(existing) = saved
      .iter()
      .find(|pending| pending.identity == metadata.identity)
    {
      if existing.size_bytes != metadata.size_bytes {
        return Err(
          "This release’s size changed. Discard its saved download before starting again.".into(),
        );
      }
    } else if saved.len() >= MAX_PENDING_DOWNLOADS {
      return Err(format!("{MAX_PENDING_DOWNLOADS} Store downloads are saved. Finish or discard one before starting another."));
    }
    write_metadata(self.metadata_path(), &metadata).await
  }

  pub(super) async fn length(&self, size: u64) -> Result<u64, String> {
    match tokio::fs::symlink_metadata(&self.0).await {
      Ok(metadata) if metadata.is_file() && metadata.len() <= size => Ok(metadata.len()),
      Ok(_) => {
        remove_if_present(&self.0).await?;
        Ok(0)
      }
      Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
      Err(_) => Err(STORAGE_ERROR.into()),
    }
  }

  pub(super) async fn finish(&self) -> Result<(), String> {
    remove_if_present(&self.0).await?;
    remove_if_present(&self.metadata_path()).await
  }
}

async fn migrate_legacy(cache: &Path) -> Result<(), String> {
  let path = cache.join(LEGACY_METADATA_FILE);
  if let Some(metadata) = read_metadata(&path).await? {
    let checkpoint = metadata.identity.checkpoint(cache);
    if checkpoint
      .part_metadata(metadata.size_bytes)
      .await?
      .is_some_and(|part| part.len() > 0)
      && checkpoint.pending_entry(cache).await?.is_none()
    {
      // Leave the old marker available if the new sidecar cannot be written.
      write_metadata(checkpoint.metadata_path(), &metadata).await?;
    }
  }
  remove_if_present(&path).await
}

async fn saved_downloads(cache: &Path) -> Result<Vec<PendingDownload>, String> {
  if !cache_present(cache).await? {
    return Ok(Vec::new());
  }
  migrate_legacy(cache).await?;
  let mut entries = tokio::fs::read_dir(cache)
    .await
    .map_err(|_| STORAGE_ERROR)?;
  let mut stems = BTreeSet::new();
  while let Some(entry) = entries.next_entry().await.map_err(|_| STORAGE_ERROR)? {
    let path = entry.path();
    if matches!(
      path.extension().and_then(|extension| extension.to_str()),
      Some("part" | "json")
    ) {
      if let Some(stem) = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| lowercase_hash(stem))
      {
        stems.insert(stem.to_owned());
      }
    }
  }

  let mut saved = Vec::new();
  for stem in stems {
    let checkpoint = DownloadCheckpoint(cache.join(format!("{stem}.part")));
    if let Some((pending, part)) = checkpoint.pending_entry(cache).await? {
      saved.push((pending, part.modified().ok()));
    } else {
      // Missing, empty or invalid files never represent a resumable download.
      checkpoint.finish().await?;
    }
  }
  saved.sort_by(|left, right| right.1.cmp(&left.1));
  Ok(saved.into_iter().map(|(pending, _)| pending).collect())
}

async fn cache_present(cache: &Path) -> Result<bool, String> {
  match tokio::fs::symlink_metadata(cache).await {
    Ok(metadata) if metadata.is_dir() => Ok(true),
    Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
    _ => Err(STORAGE_ERROR.into()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const ORIGIN: &str = "https://store.example.invalid";
  const APPLET_ID: &str = "sample";

  fn release(version: &str) -> StoreRelease {
    StoreRelease {
      version: version.into(),
      sha256: digest(version.as_bytes()),
      size_bytes: 8,
      published_at: "2026-01-01T00:00:00Z".into(),
      release_notes: String::new(),
      api_version: 2,
      permissions: Vec::new(),
      network: true,
      has_backend: false,
      download_url: release_download_path(APPLET_ID, version),
      prerelease: false,
    }
  }

  #[tokio::test]
  async fn full_cache_requires_discard_but_can_resume_any_saved_release() {
    let cache = tempfile::tempdir().unwrap();
    let cache = cache.path();
    for index in 0..MAX_PENDING_DOWNLOADS {
      let release = release(&format!("1.0.{index}"));
      let checkpoint = DownloadCheckpoint::new(cache, ORIGIN, &release);
      checkpoint
        .prepare(ORIGIN, APPLET_ID, "Sample".into(), &release)
        .await
        .unwrap();
      tokio::fs::write(&checkpoint.0, b"part").await.unwrap();
    }
    let fifth = release("2.0.0");
    let fifth_checkpoint = DownloadCheckpoint::new(cache, ORIGIN, &fifth);
    let error = fifth_checkpoint
      .prepare(ORIGIN, APPLET_ID, "Sample".into(), &fifth)
      .await
      .unwrap_err();
    assert!(error.contains("discard"));
    let pending = DownloadCheckpoint::pending_downloads(cache).await.unwrap();
    assert_eq!(pending.len(), MAX_PENDING_DOWNLOADS);
    assert!(pending
      .iter()
      .all(|download| download.downloaded_bytes == 4));

    let first = release("1.0.0");
    let first_checkpoint = DownloadCheckpoint::new(cache, ORIGIN, &first);
    first_checkpoint
      .prepare(ORIGIN, APPLET_ID, "Sample".into(), &first)
      .await
      .unwrap();
    assert_eq!(
      first_checkpoint.reviewed_received(first.size_bytes).await,
      4
    );
    let mut changed = first.clone();
    changed.size_bytes += 1;
    assert!(first_checkpoint
      .prepare(ORIGIN, APPLET_ID, "Sample".into(), &changed)
      .await
      .is_err());
    assert_eq!(
      first_checkpoint.reviewed_received(first.size_bytes).await,
      4
    );

    let discard = &pending
      .iter()
      .find(|download| download.identity.version == first.version)
      .unwrap()
      .identity;
    assert!(DownloadCheckpoint::discard(cache, discard).await.unwrap());
    fifth_checkpoint
      .prepare(ORIGIN, APPLET_ID, "Sample".into(), &fifth)
      .await
      .unwrap();
    assert_eq!(
      DownloadCheckpoint::pending_downloads(cache)
        .await
        .unwrap()
        .len(),
      MAX_PENDING_DOWNLOADS - 1
    );
    assert!(!fifth_checkpoint.metadata_path().exists());
  }

  #[tokio::test]
  async fn a_part_without_valid_metadata_is_not_adopted_by_a_new_review() {
    let cache = tempfile::tempdir().unwrap();
    let release = release("1.0.0");
    let checkpoint = DownloadCheckpoint::new(cache.path(), ORIGIN, &release);
    tokio::fs::write(&checkpoint.0, b"orphan").await.unwrap();
    assert_eq!(checkpoint.reviewed_received(release.size_bytes).await, 0);

    checkpoint
      .prepare(ORIGIN, APPLET_ID, "Sample".into(), &release)
      .await
      .unwrap();
    assert_eq!(checkpoint.length(release.size_bytes).await.unwrap(), 0);
    assert!(checkpoint.metadata_path().is_file());
  }

  #[tokio::test]
  async fn valid_legacy_checkpoint_migrates_without_replacing_other_downloads() {
    let cache = tempfile::tempdir().unwrap();
    let cache = cache.path();
    let legacy_release = release("1.0.0");
    let legacy_checkpoint = DownloadCheckpoint::new(cache, ORIGIN, &legacy_release);
    let legacy_path = cache.join(LEGACY_METADATA_FILE);
    let legacy = DownloadMetadata {
      identity: DownloadIdentity {
        origin: ORIGIN.into(),
        applet_id: APPLET_ID.into(),
        version: legacy_release.version,
        channel: StoreChannel::Stable,
        sha256: legacy_release.sha256,
      },
      name: "Original applet".into(),
      size_bytes: legacy_release.size_bytes,
    };
    write_metadata(legacy_path.clone(), &legacy).await.unwrap();
    tokio::fs::write(&legacy_checkpoint.0, b"part")
      .await
      .unwrap();
    assert_eq!(
      legacy_checkpoint.reviewed_received(legacy.size_bytes).await,
      4
    );
    assert!(legacy_path.exists());

    let release = release("2.0.0");
    let checkpoint = DownloadCheckpoint::new(cache, ORIGIN, &release);
    checkpoint
      .prepare(ORIGIN, APPLET_ID, "Other applet".into(), &release)
      .await
      .unwrap();
    tokio::fs::write(&checkpoint.0, b"new").await.unwrap();
    assert!(!legacy_path.exists());
    assert!(legacy_checkpoint.metadata_path().is_file());

    // Recover after a crash between publishing the new sidecar and removing the old marker.
    write_metadata(legacy_path.clone(), &legacy).await.unwrap();
    let saved = DownloadCheckpoint::pending_downloads(cache).await.unwrap();
    assert_eq!(saved.len(), 2);
    assert!(saved
      .iter()
      .any(|download| download.identity == legacy.identity && download.downloaded_bytes == 4));
    assert!(!legacy_path.exists());
    assert_eq!(checkpoint.reviewed_received(release.size_bytes).await, 3);
  }
}

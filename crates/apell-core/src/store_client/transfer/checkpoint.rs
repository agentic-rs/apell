use super::super::{
  model::{lowercase_hash, release_download_path, valid_applet_name},
  StoreRelease,
};
use crate::bundles::{atomic_write, digest, StoreChannel, StoreSource, MAX_BUNDLE_BYTES};
use serde::{Deserialize, Serialize};
use std::{
  io::ErrorKind,
  path::{Path, PathBuf},
};
use tokio::io::AsyncReadExt;

const METADATA_FILE: &str = "pending.json";
const MAX_METADATA_BYTES: u64 = 4_096;
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

/// The host reserves this cache for one Store download at a time. The part
/// file binds the bytes to the exact origin, release route and reviewed hash.
pub struct DownloadCheckpoint(pub(super) PathBuf);

impl DownloadCheckpoint {
  pub fn new(cache: &Path, origin: &str, release: &StoreRelease) -> Self {
    Self::for_release(cache, origin, &release.download_url, &release.sha256)
  }

  fn for_release(cache: &Path, origin: &str, download_path: &str, sha256: &str) -> Self {
    let identity = digest(format!("{origin}\n{download_path}\n{sha256}").as_bytes());
    Self(cache.join(format!("{identity}.part")))
  }

  /// Call under the same gate used to start, resume, and discard a download.
  pub async fn pending(cache: &Path) -> Result<Option<PendingDownload>, String> {
    let path = cache.join(METADATA_FILE);
    if let Some(metadata) = read_metadata(&path).await? {
      let checkpoint = metadata.identity.checkpoint(cache);
      let downloaded_bytes = checkpoint.received(metadata.size_bytes).await;
      if downloaded_bytes > 0 {
        return Ok(Some(PendingDownload {
          identity: metadata.identity,
          name: metadata.name,
          size_bytes: metadata.size_bytes,
          downloaded_bytes,
        }));
      }
    }
    // Cache eviction or interrupted preparation can leave only the metadata.
    let _ = remove_if_present(&path).await;
    Ok(None)
  }

  /// A stale request must never delete a more recent transfer.
  pub async fn discard(cache: &Path, identity: &DownloadIdentity) -> Result<bool, String> {
    let pending = Self::pending(cache).await?;
    if !pending.is_some_and(|pending| &pending.identity == identity) {
      return Ok(false);
    }
    identity.checkpoint(cache).finish().await?;
    Ok(true)
  }

  pub async fn received(&self, size: u64) -> u64 {
    match tokio::fs::symlink_metadata(&self.0).await {
      Ok(metadata) if metadata.is_file() && metadata.len() <= size => metadata.len(),
      _ => 0,
    }
  }

  #[cfg(test)]
  pub(crate) async fn prepare_for_test(&self, bytes: &[u8]) {
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
    let path = directory.join(METADATA_FILE);
    let bytes = serde_json::to_vec(&metadata).map_err(|_| STORAGE_ERROR)?;
    tokio::task::spawn_blocking(move || atomic_write(&path, &bytes))
      .await
      .map_err(|_| STORAGE_ERROR)?
      .map_err(|_| STORAGE_ERROR)?;

    let mut entries = tokio::fs::read_dir(directory)
      .await
      .map_err(|_| STORAGE_ERROR)?;
    // Retain only the latest download the user chose to bound the cache size.
    while let Some(entry) = entries.next_entry().await.map_err(|_| STORAGE_ERROR)? {
      let path = entry.path();
      if path != self.0
        && path
          .extension()
          .is_some_and(|extension| extension == "part")
      {
        remove_if_present(&path).await?;
      }
    }
    Ok(())
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
    let directory = self.0.parent().ok_or(STORAGE_ERROR)?;
    let _ = remove_if_present(&directory.join(METADATA_FILE)).await;
    Ok(())
  }
}

use super::{verify_download, StoreClient, StoreRelease};
use crate::bundles::digest;
use reqwest::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_RANGE, ETAG, IF_RANGE, RANGE};
use std::{
  future::Future,
  io::ErrorKind,
  path::{Path, PathBuf},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub enum DownloadOutcome {
  Verified(Vec<u8>),
  Paused { received_bytes: u64 },
}

/// The host reserves this cache for one Store download at a time. The file
/// name binds the bytes to the exact origin, release route and reviewed hash.
pub struct DownloadCheckpoint(PathBuf);

impl DownloadCheckpoint {
  pub fn new(cache: &Path, origin: &str, release: &StoreRelease) -> Self {
    let identity =
      digest(format!("{origin}\n{}\n{}", release.download_url, release.sha256).as_bytes());
    Self(cache.join(format!("{identity}.part")))
  }

  pub async fn received(&self, size: u64) -> u64 {
    match tokio::fs::symlink_metadata(&self.0).await {
      Ok(meta) if meta.is_file() && meta.len() <= size => meta.len(),
      _ => 0,
    }
  }

  #[cfg(test)]
  pub(crate) async fn prepare_for_test(&self, bytes: &[u8]) {
    self.prepare().await.unwrap();
    tokio::fs::write(&self.0, bytes).await.unwrap();
  }

  async fn prepare(&self) -> Result<(), String> {
    let directory = self.0.parent().ok_or(STORAGE_ERROR)?;
    tokio::fs::create_dir_all(directory)
      .await
      .map_err(|_| STORAGE_ERROR)?;
    let mut entries = tokio::fs::read_dir(directory)
      .await
      .map_err(|_| STORAGE_ERROR)?;
    // Retain only the latest download the user chose. This bounds disk usage
    // even when many releases are tried and interrupted.
    while let Some(entry) = entries.next_entry().await.map_err(|_| STORAGE_ERROR)? {
      let path = entry.path();
      if path != self.0 && path.extension().is_some_and(|ext| ext == "part") {
        tokio::fs::remove_file(path)
          .await
          .map_err(|_| STORAGE_ERROR)?;
      }
    }
    Ok(())
  }

  async fn length(&self, size: u64) -> Result<u64, String> {
    match tokio::fs::symlink_metadata(&self.0).await {
      Ok(meta) if meta.is_file() && meta.len() <= size => Ok(meta.len()),
      Ok(_) => {
        self.remove().await?;
        Ok(0)
      }
      Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
      Err(_) => Err(STORAGE_ERROR.into()),
    }
  }

  async fn remove(&self) -> Result<(), String> {
    match tokio::fs::remove_file(&self.0).await {
      Ok(_) => Ok(()),
      Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
      Err(_) => Err(STORAGE_ERROR.into()),
    }
  }
}

const STORAGE_ERROR: &str = "Could not access the Store download cache.";

impl StoreClient {
  pub async fn download_resumable(
    &self,
    applet_id: &str,
    selected: &StoreRelease,
    checkpoint: &DownloadCheckpoint,
    pause: impl Future<Output = ()>,
    progress: impl Fn(u64, Option<u64>),
  ) -> Result<DownloadOutcome, String> {
    selected.validate(applet_id)?;
    let size = selected.size_bytes;
    let mut received = checkpoint.received(size).await;
    let paused = |received_bytes| Ok(DownloadOutcome::Paused { received_bytes });
    tokio::pin!(pause);
    tokio::select! {
      biased;
      _ = &mut pause => return paused(received),
      result = self.review_download(applet_id, selected) => result?,
    }
    checkpoint.prepare().await?;
    received = checkpoint.length(size).await?;
    let etag = format!("\"{}\"", selected.sha256);
    progress(received, Some(size));
    if received < size {
      let mut request = self
        .client
        .get(format!("{}{}", self.origin, selected.download_url))
        .header(ACCEPT_ENCODING, "identity");
      if received > 0 {
        request = request
          .header(RANGE, format!("bytes={received}-"))
          .header(IF_RANGE, &etag);
      }
      let response = tokio::select! {
        biased;
        _ = &mut pause => return paused(received),
        response = request.send() => response,
      };
      let Ok(mut response) = response else {
        return paused(received);
      };
      if response
        .headers()
        .get(CONTENT_ENCODING)
        .is_some_and(|value| value != "identity")
      {
        return Err("The Store returned an unsupported package encoding.".into());
      }
      let append = match response.status().as_u16() {
        200 => {
          received = 0;
          false
        }
        206 if received > 0 => {
          let range = format!("bytes {received}-{}/{size}", size - 1);
          if !response
            .headers()
            .get(CONTENT_RANGE)
            .is_some_and(|value| value == range.as_str())
            || !response
              .headers()
              .get(ETAG)
              .is_some_and(|value| value == etag.as_str())
          {
            return Err("The Store returned an invalid download range.".into());
          }
          true
        }
        301 | 302 | 303 | 307 | 308 => {
          return Err("The store returned a redirect. Update its address before continuing.".into())
        }
        404 => return Err("This applet or release is no longer available in the store.".into()),
        429 | 503 => return Err("The store is busy. Try again shortly.".into()),
        code => return Err(format!("The store returned HTTP {code}.")),
      };
      if response
        .content_length()
        .is_some_and(|length| length != size - received)
      {
        return Err("The Store returned a package with an unexpected size.".into());
      }
      let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&checkpoint.0)
        .await
        .map_err(|_| STORAGE_ERROR)?;
      progress(received, Some(size));
      loop {
        let chunk = tokio::select! {
          biased;
          _ = &mut pause => break,
          chunk = response.chunk() => chunk,
        };
        let Ok(Some(chunk)) = chunk else {
          break;
        };
        if chunk.len() as u64 > size - received {
          drop(file);
          checkpoint.remove().await?;
          return Err("The Store response exceeds the reviewed package size.".into());
        }
        file.write_all(&chunk).await.map_err(|_| STORAGE_ERROR)?;
        received += chunk.len() as u64;
        progress(received, Some(size));
      }
      file.flush().await.map_err(|_| STORAGE_ERROR)?;
      if received != size {
        return paused(received);
      }
    }
    let mut bytes = Vec::new();
    tokio::fs::File::open(&checkpoint.0)
      .await
      .map_err(|_| STORAGE_ERROR)?
      .take(size + 1)
      .read_to_end(&mut bytes)
      .await
      .map_err(|_| STORAGE_ERROR)?;
    let result = verify_download(bytes, applet_id, selected).await;
    checkpoint.remove().await?;
    result.map(DownloadOutcome::Verified)
  }
}

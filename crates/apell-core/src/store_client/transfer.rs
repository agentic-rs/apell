mod checkpoint;

use super::{verify_download, StoreClient, StoreRelease};
use checkpoint::STORAGE_ERROR;
pub use checkpoint::{DownloadCheckpoint, DownloadIdentity, PendingDownload};
use reqwest::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_RANGE, ETAG, IF_RANGE, RANGE};
use std::future::Future;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub enum DownloadOutcome {
  Verified(Vec<u8>),
  Paused { received_bytes: u64 },
}

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
    let mut received = checkpoint.reviewed_received(size).await;
    let paused = |received_bytes| Ok(DownloadOutcome::Paused { received_bytes });
    tokio::pin!(pause);
    let name = tokio::select! {
      biased;
      _ = &mut pause => return paused(received),
      result = self.review_download(applet_id, selected) => result?,
    };
    checkpoint
      .prepare(&self.origin, applet_id, name, selected)
      .await?;
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
          checkpoint.finish().await?;
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
    checkpoint.finish().await?;
    result.map(DownloadOutcome::Verified)
  }
}

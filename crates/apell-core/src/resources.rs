use crate::{
  bundles::{atomic_write, digest, valid_hash},
  valid_name,
};
use reqwest::Client;
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};

pub const MAX_RESOURCE_BYTES: usize = 32 * 1024 * 1024;

pub struct ResourceStore {
  root: PathBuf,
  client: Client,
}

pub fn validate_url(input: &str) -> Result<url::Url, String> {
  let url = url::Url::parse(input).map_err(|_| "Invalid download URL.")?;
  let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
  if (url.scheme() != "https" && !(url.scheme() == "http" && local))
    || !url.username().is_empty()
    || url.password().is_some()
  {
    return Err(
      "Use HTTPS, or loopback HTTP for local development. URL credentials are not supported."
        .into(),
    );
  }
  Ok(url)
}

impl ResourceStore {
  pub fn new(root: PathBuf) -> Result<Self, String> {
    let client = crate::http::client_builder()?
      .redirect(reqwest::redirect::Policy::none())
      .connect_timeout(Duration::from_secs(15))
      .timeout(Duration::from_secs(120))
      .build()
      .map_err(|_| "Could not initialize downloads.")?;
    Ok(Self { root, client })
  }

  pub async fn fetch(
    &self,
    url: &str,
    limit: usize,
    progress: impl Fn(u64, Option<u64>),
  ) -> Result<Vec<u8>, String> {
    let url = validate_url(url)?;
    let mut response = self
      .client
      .get(url)
      .send()
      .await
      .map_err(|_| "Download connection failed.")?;
    if !response.status().is_success() {
      return Err(format!(
        "Download returned HTTP {}.",
        response.status().as_u16()
      ));
    }
    let total = response.content_length();
    if total.is_some_and(|n| n > limit as u64) {
      return Err("Download exceeds the size limit.".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
      .chunk()
      .await
      .map_err(|_| "Download was interrupted.")?
    {
      if bytes.len() + chunk.len() > limit {
        return Err("Download exceeds the size limit.".into());
      }
      bytes.extend_from_slice(&chunk);
      progress(bytes.len() as u64, total);
    }
    Ok(bytes)
  }

  pub async fn download(
    &self,
    applet_id: &str,
    url: &str,
    expected_hash: Option<&str>,
    progress: impl Fn(u64, Option<u64>),
  ) -> Result<Value, String> {
    if !valid_name(applet_id) {
      return Err("Invalid applet identifier.".into());
    }
    let bytes = self.fetch(url, MAX_RESOURCE_BYTES, progress).await?;
    let resource_id = digest(&bytes);
    if expected_hash.is_some_and(|h| !h.eq_ignore_ascii_case(&resource_id)) {
      return Err("Resource checksum does not match.".into());
    }
    let path = self.root.join(applet_id).join(&resource_id);
    let size_bytes = bytes.len();
    tokio::task::spawn_blocking(move || atomic_write(&path, &bytes))
      .await
      .map_err(|_| "Resource save was interrupted.")??;
    Ok(json!({"resource_id":resource_id,"size_bytes":size_bytes}))
  }

  pub async fn read(
    &self,
    applet_id: &str,
    resource_id: &str,
    offset: u64,
    length: usize,
  ) -> Result<Value, String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    if !valid_name(applet_id) || !valid_hash(resource_id) || length > 1024 * 1024 {
      return Err("Invalid resource request; reads are limited to 1 MiB.".into());
    }
    let mut file = tokio::fs::File::open(self.root.join(applet_id).join(resource_id))
      .await
      .map_err(|_| "Resource is not available.")?;
    file
      .seek(std::io::SeekFrom::Start(offset))
      .await
      .map_err(|_| "Could not seek resource.")?;
    let mut bytes = vec![0; length];
    let mut read = 0;
    while read < length {
      let count = file
        .read(&mut bytes[read..])
        .await
        .map_err(|_| "Could not read resource.")?;
      if count == 0 {
        break;
      }
      read += count;
    }
    bytes.truncate(read);
    Ok(json!({"bytes":bytes}))
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn only_secure_or_local_downloads() {
    assert!(validate_url("https://example.invalid/bundle").is_ok());
    assert!(validate_url("http://127.0.0.1:8080/bundle").is_ok());
    assert!(validate_url("file:///etc/passwd").is_err());
    assert!(validate_url("http://example.invalid/bundle").is_err());
    assert!(validate_url("https://user:secret@example.invalid/bundle").is_err());
  }
  #[tokio::test]
  async fn resources_are_scoped_and_reads_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let store = ResourceStore::new(dir.path().into()).unwrap();
    let id = digest(b"hello");
    atomic_write(&dir.path().join("first").join(&id), b"hello").unwrap();
    assert_eq!(
      store.read("first", &id, 1, 3).await.unwrap()["bytes"],
      json!([101, 108, 108])
    );
    assert!(store.read("second", &id, 0, 3).await.is_err());
    assert!(store.read("first", "../../escape", 0, 3).await.is_err());
    assert!(store.read("first", &id, 0, 1024 * 1024 + 1).await.is_err());
  }
}

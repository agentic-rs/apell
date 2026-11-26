//! Native, credential-free access to one explicitly configured applet store.
mod model;
mod transfer;
pub use model::{StoreApplet, StoreCatalog, StoreDetail, StoreRelease};
pub use transfer::{DownloadCheckpoint, DownloadIdentity, DownloadOutcome, PendingDownload};

use crate::bundles::{digest, parse};
use crate::icons::{png_data_url, MAX_ICON_BYTES};
use reqwest::{Client, ClientBuilder, Response};
use serde::de::DeserializeOwned;
use std::time::Duration;
use url::{Host, Url};

pub fn normalize_store_url(value: &str) -> Result<String, String> {
  let invalid = || {
    "Use a root HTTPS store address without credentials, a query, or a fragment. HTTP is only available on loopback.".to_owned()
  };
  if value.is_empty()
    || value.len() > 2_048
    || value
      .chars()
      .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
  {
    return Err(invalid());
  }
  let authority = value.split_once("://").ok_or_else(invalid)?.1;
  let authority_end = authority.find(['/', '?', '#']).unwrap_or(authority.len());
  if authority[..authority_end].contains('@') || !matches!(&authority[authority_end..], "" | "/") {
    return Err(invalid());
  }
  let url = Url::parse(value).map_err(|_| invalid())?;
  let loopback = match url.host() {
    Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
    Some(Host::Ipv4(ip)) => ip.is_loopback(),
    Some(Host::Ipv6(ip)) => ip.is_loopback(),
    None => false,
  };
  if !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
    || url.host_str().is_none()
    || !url.username().is_empty()
    || url.password().is_some()
    || url.query().is_some()
    || url.fragment().is_some()
    || url.path() != "/"
  {
    return Err(invalid());
  }
  Ok(url.origin().ascii_serialization())
}

pub struct StoreClient {
  origin: String,
  client: Client,
}

impl StoreClient {
  pub fn new(store_url: &str) -> Result<Self, String> {
    Self::from_origin(
      normalize_store_url(store_url)?,
      crate::http::client_builder()?,
    )
  }

  /// Uses the host's TLS policy and applies the Store's timeout and redirect rules.
  /// The builder must not carry credentials or default authorization headers.
  pub fn with_http_builder(store_url: &str, builder: ClientBuilder) -> Result<Self, String> {
    Self::from_origin(normalize_store_url(store_url)?, builder)
  }

  fn from_origin(origin: String, builder: ClientBuilder) -> Result<Self, String> {
    let mut builder = builder
      .redirect(reqwest::redirect::Policy::none())
      .no_gzip()
      .no_brotli()
      .no_deflate()
      .no_zstd()
      .connect_timeout(Duration::from_secs(15))
      .read_timeout(Duration::from_secs(30))
      .timeout(Duration::from_secs(120))
      .user_agent("Apell");
    if origin.starts_with("http:") {
      builder = builder.no_proxy();
    }
    Ok(Self {
      origin,
      client: builder
        .build()
        .map_err(|_| "Could not initialize store networking.")?,
    })
  }

  async fn response(&self, path: &str) -> Result<Response, String> {
    let response = self
      .client
      .get(format!("{}{path}", self.origin))
      .send()
      .await
      .map_err(|_| "Could not reach the store. Check the address and connection.")?;
    match response.status().as_u16() {
      200 => Ok(response),
      301 | 302 | 303 | 307 | 308 => {
        Err("The store returned a redirect. Update its address before continuing.".into())
      }
      404 => Err("This applet or release is no longer available in the store.".into()),
      429 | 503 => Err("The store is busy. Try again shortly.".into()),
      code => Err(format!("The store returned HTTP {code}.")),
    }
  }

  async fn json<T: DeserializeOwned>(&self, path: &str, limit: usize) -> Result<T, String> {
    let bytes = bounded_body(self.response(path).await?, limit, |_, _| {}).await?;
    serde_json::from_slice(&bytes).map_err(|_| "The store returned invalid JSON.".into())
  }

  pub async fn catalog(&self) -> Result<StoreCatalog, String> {
    let catalog: StoreCatalog = self.json("/api/v1/applets", 8 * 1024 * 1024).await?;
    catalog.validate()?;
    Ok(catalog)
  }

  pub async fn detail(&self, applet_id: &str) -> Result<StoreDetail, String> {
    if !crate::valid_name(applet_id) {
      return Err("Invalid applet identity.".into());
    }
    let mut detail: StoreDetail = self
      .json(&format!("/api/v1/applets/{applet_id}"), 16 * 1024 * 1024)
      .await?;
    detail.validate_and_fill_nightly_release(applet_id)?;
    Ok(detail)
  }

  pub async fn download(
    &self,
    applet_id: &str,
    selected: &StoreRelease,
    progress: impl Fn(u64, Option<u64>),
  ) -> Result<Vec<u8>, String> {
    self.review_download(applet_id, selected).await?;
    let bytes = bounded_body(
      self.response(&selected.download_url).await?,
      selected.size_bytes as usize,
      |received, _| progress(received, Some(selected.size_bytes)),
    )
    .await?;
    verify_download(bytes, applet_id, selected).await
  }

  async fn review_download(
    &self,
    applet_id: &str,
    selected: &StoreRelease,
  ) -> Result<String, String> {
    selected.validate(applet_id)?;
    let detail = self.detail(applet_id).await?;
    let current = detail
      .releases
      .iter()
      .find(|release| release.version == selected.version)
      .ok_or("This version is no longer available. Refresh its details.")?;
    if current != selected {
      return Err("The selected release changed. Review its details again.".into());
    }
    Ok(detail.applet.name)
  }

  pub async fn icon(&self, icon_url: &str) -> Result<String, String> {
    let hash = model::icon_hash(icon_url)?;
    let bytes = bounded_body(self.response(icon_url).await?, MAX_ICON_BYTES, |_, _| {}).await?;
    if digest(&bytes) != hash {
      return Err("The store icon failed verification.".into());
    }
    png_data_url(&bytes)
  }
}

async fn verify_download(
  bytes: Vec<u8>,
  applet_id: &str,
  selected: &StoreRelease,
) -> Result<Vec<u8>, String> {
  if bytes.len() as u64 != selected.size_bytes || digest(&bytes) != selected.sha256 {
    return Err("The downloaded package does not match the selected release.".into());
  }
  let id = applet_id.to_owned();
  let release = selected.clone();
  tokio::task::spawn_blocking(move || {
    let package = parse(&bytes)?;
    let manifest = package.manifest;
    if manifest.applet_id != id
      || manifest.version != release.version
      || manifest.api_version != release.api_version
      || manifest.permissions != release.permissions
      || manifest.network != release.network
      || manifest.lib.is_some() != release.has_backend
    {
      return Err("The package identity or permissions do not match the reviewed release.".into());
    }
    Ok(bytes)
  })
  .await
  .map_err(|_| "Package validation was interrupted.".to_owned())?
}

async fn bounded_body(
  mut response: Response,
  limit: usize,
  progress: impl Fn(u64, Option<u64>),
) -> Result<Vec<u8>, String> {
  let length = response.content_length();
  if length.is_some_and(|size| size > limit as u64) {
    return Err("The store response exceeds its size limit.".into());
  }
  let mut bytes = Vec::new();
  while let Some(chunk) = response
    .chunk()
    .await
    .map_err(|_| "The store download was interrupted.")?
  {
    if chunk.len() > limit.saturating_sub(bytes.len()) {
      return Err("The store response exceeds its size limit.".into());
    }
    bytes.extend_from_slice(&chunk);
    progress(bytes.len() as u64, length);
  }
  Ok(bytes)
}

#[cfg(test)]
mod tests;

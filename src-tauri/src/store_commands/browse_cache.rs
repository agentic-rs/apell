use crate::host::Host;
use apell_core::{
  bundles::atomic_write,
  store_client::{StoreCatalog, StoreDetail},
};
use serde::{Deserialize, Serialize};
use std::{
  collections::HashMap,
  fs,
  io::Read,
  path::Path,
  sync::{Arc, Mutex},
};

const CACHE_FILE: &str = "store-browse-v1.json";
const MAX_ORIGINS: usize = 4;
const MAX_DETAILS: usize = 8;
const MAX_CATALOG_BYTES: usize = 1_500_000;
const MAX_DETAIL_BYTES: usize = 200_000;
const MAX_CACHE_BYTES: usize = 4_000_000;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(crate) struct BrowseCache {
  in_flight: Mutex<HashMap<CacheKey, PendingObservations>>,
  disk: Mutex<()>,
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct CacheKey {
  origin: String,
  applet_id: Option<String>,
}

#[derive(Default)]
struct PendingObservations {
  count: usize,
  issued: u64,
  observed: u64,
}

pub(super) struct Observation {
  cache: Arc<BrowseCache>,
  key: CacheKey,
  order: u64,
}

pub(super) enum Metadata {
  Catalog(StoreCatalog),
  Detail(StoreDetail),
}

impl BrowseCache {
  pub(super) fn observe(
    self: &Arc<Self>,
    origin: &str,
    applet_id: Option<&str>,
  ) -> Option<Observation> {
    let key = CacheKey {
      origin: origin.into(),
      applet_id: applet_id.map(str::to_owned),
    };
    let mut in_flight = self.in_flight.lock().ok()?;
    let pending = in_flight.entry(key.clone()).or_default();
    pending.issued = pending.issued.checked_add(1)?;
    pending.count += 1;
    Some(Observation {
      cache: self.clone(),
      key,
      order: pending.issued,
    })
  }
}

impl Observation {
  pub(super) async fn remember(self, host: &Host, metadata: Metadata) {
    let root = host.root.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || self.publish(&root, metadata)).await;
  }

  fn publish(&self, root: &Path, mut metadata: Metadata) {
    let Ok(_disk) = self.cache.disk.lock() else {
      return;
    };
    {
      let Ok(mut in_flight) = self.cache.in_flight.lock() else {
        return;
      };
      let Some(pending) = in_flight.get_mut(&self.key) else {
        return;
      };
      if pending.observed >= self.order {
        return;
      }
      // Even an oversized successful response supersedes an older overlapping
      // response. A failed request never advances the observed order.
      pending.observed = self.order;
    }
    if !metadata.prepare(&self.key) {
      return;
    }
    let mut snapshot = read_cache(root);
    let mut origin = match snapshot
      .origins
      .iter()
      .position(|origin| origin.store_url == self.key.origin)
    {
      Some(index) => snapshot.origins.remove(index),
      None => CachedOrigin {
        store_url: self.key.origin.clone(),
        catalog: None,
        details: Vec::new(),
      },
    };
    match metadata {
      Metadata::Catalog(catalog) => origin.catalog = Some(catalog),
      Metadata::Detail(detail) => {
        origin
          .details
          .retain(|entry| entry.applet.applet_id != detail.applet.applet_id);
        origin.details.insert(0, detail);
        origin.details.truncate(MAX_DETAILS);
      }
    }
    snapshot.origins.insert(0, origin);
    snapshot.origins.truncate(MAX_ORIGINS);
    snapshot.write(root);
  }
}

impl Drop for Observation {
  fn drop(&mut self) {
    if let Ok(mut in_flight) = self.cache.in_flight.lock() {
      if let Some(pending) = in_flight.get_mut(&self.key) {
        pending.count -= 1;
        if pending.count == 0 {
          in_flight.remove(&self.key);
        }
      }
    }
  }
}

impl Metadata {
  fn prepare(&mut self, key: &CacheKey) -> bool {
    match (self, &key.applet_id) {
      (Self::Catalog(catalog), None) => {
        // Browse cards use release identification only. The detail cache keeps
        // the full public release history and notes.
        for applet in &mut catalog.applets {
          for release in std::iter::once(&mut applet.latest_release)
            .chain(applet.latest_stable_release.as_mut())
            .chain(applet.latest_nightly_release.as_mut())
          {
            release.release_notes.clear();
          }
        }
        fits(catalog, MAX_CATALOG_BYTES)
      }
      (Self::Detail(detail), Some(id)) => {
        detail.applet.applet_id == *id && fits(detail, MAX_DETAIL_BYTES)
      }
      _ => false,
    }
  }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
  origins: Vec<CachedOrigin>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CachedOrigin {
  store_url: String,
  catalog: Option<StoreCatalog>,
  details: Vec<StoreDetail>,
}

impl Snapshot {
  fn write(mut self, root: &Path) {
    while !self.origins.is_empty() {
      let Ok(bytes) = serde_json::to_vec(&self) else {
        return;
      };
      if bytes.len() <= MAX_CACHE_BYTES {
        let _ = atomic_write(&root.join(CACHE_FILE), &bytes);
        return;
      }
      self.origins.pop();
    }
  }
}

fn fits(value: &impl Serialize, limit: usize) -> bool {
  serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= limit)
}

fn read_cache(root: &Path) -> Snapshot {
  let path = root.join(CACHE_FILE);
  if !fs::symlink_metadata(&path)
    .is_ok_and(|entry| entry.is_file() && entry.len() <= MAX_CACHE_BYTES as u64)
  {
    return Snapshot::default();
  }
  let mut bytes = Vec::new();
  let read = fs::File::open(path).and_then(|file| {
    file
      .take((MAX_CACHE_BYTES + 1) as u64)
      .read_to_end(&mut bytes)
  });
  if read.is_err() || bytes.len() > MAX_CACHE_BYTES {
    return Snapshot::default();
  }
  serde_json::from_slice::<Snapshot>(&bytes)
    .ok()
    .filter(|cache| {
      cache.origins.len() <= MAX_ORIGINS
        && cache
          .origins
          .iter()
          .all(|origin| origin.details.len() <= MAX_DETAILS)
    })
    .unwrap_or_default()
}

async fn saved_origin(host: &Host, store_url: &str) -> Option<CachedOrigin> {
  let root = host.root.clone();
  let store_url = store_url.to_owned();
  tauri::async_runtime::spawn_blocking(move || {
    read_cache(&root)
      .origins
      .into_iter()
      .find(|origin| origin.store_url == store_url)
  })
  .await
  .ok()
  .flatten()
}

pub(super) async fn saved_catalog(host: &Host, store_url: &str) -> Option<StoreCatalog> {
  saved_origin(host, store_url)
    .await?
    .catalog
    .filter(|catalog| catalog.validate().is_ok() && fits(catalog, MAX_CATALOG_BYTES))
}

pub(super) async fn saved_detail(
  host: &Host,
  store_url: &str,
  applet_id: &str,
) -> Option<StoreDetail> {
  saved_origin(host, store_url)
    .await?
    .details
    .into_iter()
    .find(|detail| {
      detail.applet.applet_id == applet_id
        && detail.validate(applet_id).is_ok()
        && fits(detail, MAX_DETAIL_BYTES)
    })
}

use super::{installed, installed_channel_release, is_newer, store_client, Host};
use apell_core::{
  bundles::{
    atomic_write, AppletListReport, InstalledApplet, PackageSource, StoreChannel, StoreSource,
  },
  store_client::StoreCatalog,
};
use futures_util::{stream, StreamExt};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
  collections::{BTreeMap, BTreeSet},
  fs,
  future::Future,
  io::Read,
  path::Path,
  time::Duration,
};

const MAX_UPDATE_REQUESTS: usize = 4;
const UPDATE_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const CACHE_FILE: &str = "store-updates-v1.json";
const MAX_CACHED_UPDATES: usize = 256;
const MAX_CACHE_BYTES: usize = 1024 * 1024;

#[derive(Serialize)]
pub(super) struct UpdatesResult {
  pub(super) sources: Vec<UpdateSource>,
}

#[derive(Serialize)]
pub(super) struct UpdateSource {
  pub(super) store_url: String,
  pub(super) installed_count: usize,
  pub(super) checked_count: usize,
  pub(super) updates: Vec<AppletUpdate>,
  pub(super) saved_updates: Vec<AppletUpdate>,
  pub(super) unresolved_applets: Vec<UnresolvedUpdate>,
  #[serde(skip_serializing_if = "Option::is_none")]
  error: Option<String>,
}

#[derive(Serialize)]
pub(super) struct AppletUpdate {
  pub(super) applet: InstalledApplet,
  pub(super) version: String,
}

#[derive(Serialize)]
pub(super) struct UnresolvedUpdate {
  pub(super) applet: InstalledApplet,
  pub(super) reason: UnresolvedReason,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum UnresolvedReason {
  NotChecked,
  ReleaseUnavailable,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SavedUpdate {
  pub(super) source: StoreSource,
  pub(super) sha256: String,
  pub(super) version: String,
}

pub(super) type SavedUpdates = BTreeMap<String, SavedUpdate>;
type Catalogs = BTreeMap<String, Result<StoreCatalog, String>>;

enum ReportMode {
  Live,
  Saved,
}

pub(super) async fn saved(host: &Host) -> Result<UpdatesResult, String> {
  let saved = read_saved(host).await;
  if saved.is_empty() {
    return Ok(UpdatesResult { sources: vec![] });
  }
  Ok(saved_report(installed(host).await?, &saved))
}

pub(super) fn saved_report(report: AppletListReport, saved: &SavedUpdates) -> UpdatesResult {
  let mut report = update_report(report, Catalogs::new(), saved, ReportMode::Saved);
  report
    .sources
    .retain(|source| !source.saved_updates.is_empty());
  report
}

pub(super) async fn check(host: &Host) -> Result<UpdatesResult, String> {
  check_scope(host, None).await
}

pub(super) async fn retry(host: &Host, store_url: &str) -> Result<Option<UpdateSource>, String> {
  Ok(
    check_scope(host, Some(store_url))
      .await?
      .sources
      .into_iter()
      .find(|source| source.store_url == store_url),
  )
}

async fn check_scope(host: &Host, store_url: Option<&str>) -> Result<UpdatesResult, String> {
  // Serialize checks through cache publication. Installations and saved reads
  // use different gates and can continue during the network wait.
  let check = host.store_updates_gate.clone().lock_owned().await;
  let earlier = read_saved(host).await;
  let result = check_updates(
    || installed(host),
    |store_url| async move {
      let client = store_client(host, &store_url)?;
      tokio::time::timeout(UPDATE_REQUEST_TIMEOUT, client.catalog())
        .await
        .map_err(|_| "The store took too long to respond. Try again shortly.")?
    },
    &earlier,
    store_url,
  )
  .await?;
  let next = cache_projection(&result, store_url);
  if next != earlier {
    let root = host.root.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
      let _check = check;
      write_cache(&root, &next);
    })
    .await;
  }
  Ok(result)
}

pub(super) async fn check_updates<F, R>(
  read_installed: impl Fn() -> R,
  fetch_catalog: impl Fn(String) -> F,
  saved: &SavedUpdates,
  store_url: Option<&str>,
) -> Result<UpdatesResult, String>
where
  F: Future<Output = Result<StoreCatalog, String>>,
  R: Future<Output = Result<AppletListReport, String>>,
{
  let initial = read_installed().await?;
  let origins: BTreeSet<_> = initial
    .applets
    .iter()
    .filter_map(store_source)
    .filter(|source| store_url.is_none_or(|url| source.store_url == url))
    .map(|source| source.store_url.clone())
    .collect();
  if origins.is_empty() {
    return Ok(update_report(
      initial,
      Catalogs::new(),
      saved,
      ReportMode::Live,
    ));
  }
  let fetch_catalog = &fetch_catalog;
  let catalogs = stream::iter(origins)
    .map(|origin| async move {
      let check = fetch_catalog(origin.clone()).await;
      (origin, check)
    })
    .buffer_unordered(MAX_UPDATE_REQUESTS)
    .collect()
    .await;
  // Installations can change while a Store responds. Compare against the
  // current source, package, and channel before reporting or saving a result.
  Ok(update_report(
    read_installed().await?,
    catalogs,
    saved,
    ReportMode::Live,
  ))
}

fn installed_by_store(report: AppletListReport) -> BTreeMap<String, Vec<InstalledApplet>> {
  let mut installed = BTreeMap::<String, Vec<InstalledApplet>>::new();
  for applet in report.applets {
    let Some(source) = store_source(&applet) else {
      continue;
    };
    installed
      .entry(source.store_url.clone())
      .or_default()
      .push(applet);
  }
  installed
}

fn store_source(applet: &InstalledApplet) -> Option<&StoreSource> {
  match applet.source.as_ref()? {
    PackageSource::Store(source)
      if source.applet_id == applet.manifest.applet_id && source.validate().is_ok() =>
    {
      Some(source)
    }
    _ => None,
  }
}

fn update_report(
  report: AppletListReport,
  mut catalogs: Catalogs,
  saved: &SavedUpdates,
  mode: ReportMode,
) -> UpdatesResult {
  let sources = installed_by_store(report)
    .into_iter()
    .map(|(store_url, applets)| {
      let check = catalogs.remove(&store_url);
      let mut source = UpdateSource {
        store_url,
        installed_count: applets.len(),
        checked_count: 0,
        updates: Vec::new(),
        saved_updates: Vec::new(),
        unresolved_applets: Vec::new(),
        error: None,
      };
      match check {
        Some(Ok(catalog)) => {
          let listings: BTreeMap<_, _> = catalog
            .applets
            .iter()
            .map(|listing| (listing.applet_id.as_str(), listing))
            .collect();
          for applet in applets {
            let Some(release) = listings
              .get(applet.manifest.applet_id.as_str())
              .and_then(|listing| installed_channel_release(listing, &applet))
            else {
              source.unresolved_applets.push(UnresolvedUpdate {
                applet,
                reason: UnresolvedReason::ReleaseUnavailable,
              });
              continue;
            };
            source.checked_count += 1;
            if is_newer(release, &applet) {
              source.updates.push(AppletUpdate {
                applet,
                version: release.version.clone(),
              });
            }
          }
        }
        failed => {
          source.error = failed.and_then(Result::err);
          for applet in applets {
            if let Some(version) = saved_update_version(&applet, saved) {
              source.saved_updates.push(AppletUpdate {
                applet,
                version: version.to_owned(),
              });
            } else if matches!(mode, ReportMode::Live) {
              source.unresolved_applets.push(UnresolvedUpdate {
                applet,
                reason: UnresolvedReason::NotChecked,
              });
            }
          }
        }
      }
      source
    })
    .collect();
  UpdatesResult { sources }
}

fn saved_update_version<'a>(applet: &InstalledApplet, saved: &'a SavedUpdates) -> Option<&'a str> {
  let source = store_source(applet)?;
  let hint = saved.get(&applet.manifest.applet_id)?;
  if &hint.source != source || hint.sha256 != applet.sha256 || hint.version.len() > 80 {
    return None;
  }
  let version = Version::parse(&hint.version).ok()?;
  let installed = Version::parse(&applet.manifest.version).ok()?;
  (source.channel == StoreChannel::from_prerelease(!version.pre.is_empty())
    && version.cmp_precedence(&installed).is_gt())
  .then_some(hint.version.as_str())
}

pub(super) fn cache_projection(report: &UpdatesResult, store_url: Option<&str>) -> SavedUpdates {
  let is_retry_source = |source: &&UpdateSource| Some(source.store_url.as_str()) == store_url;
  report
    .sources
    .iter()
    // A targeted check can fill the remaining space without evicting valid
    // hints from other sources. Their installations were revalidated above.
    .filter(|source| !is_retry_source(source))
    .chain(report.sources.iter().filter(is_retry_source))
    .flat_map(|source| source.updates.iter().chain(&source.saved_updates))
    .filter_map(|entry| {
      let source = store_source(&entry.applet)?.clone();
      let id = source.applet_id.clone();
      Some((
        id,
        SavedUpdate {
          source,
          sha256: entry.applet.sha256.clone(),
          version: entry.version.clone(),
        },
      ))
    })
    .take(MAX_CACHED_UPDATES)
    .collect()
}

async fn read_saved(host: &Host) -> SavedUpdates {
  let root = host.root.clone();
  tauri::async_runtime::spawn_blocking(move || read_cache(&root))
    .await
    .unwrap_or_default()
}

pub(super) fn read_cache(root: &Path) -> SavedUpdates {
  let path = root.join(CACHE_FILE);
  if !fs::symlink_metadata(&path)
    .is_ok_and(|entry| entry.is_file() && entry.len() <= MAX_CACHE_BYTES as u64)
  {
    return SavedUpdates::new();
  }
  let mut bytes = Vec::new();
  let read = fs::File::open(path).and_then(|file| {
    file
      .take((MAX_CACHE_BYTES + 1) as u64)
      .read_to_end(&mut bytes)
  });
  if read.is_err() || bytes.len() > MAX_CACHE_BYTES {
    return SavedUpdates::new();
  }
  serde_json::from_slice::<Vec<SavedUpdate>>(&bytes)
    .ok()
    .filter(|entries| entries.len() <= MAX_CACHED_UPDATES)
    .unwrap_or_default()
    .into_iter()
    .map(|entry| (entry.source.applet_id.clone(), entry))
    .collect()
}

pub(super) fn write_cache(root: &Path, cache: &SavedUpdates) {
  if let Ok(bytes) = serde_json::to_vec(&cache.values().collect::<Vec<_>>()) {
    if bytes.len() <= MAX_CACHE_BYTES {
      let _ = atomic_write(&root.join(CACHE_FILE), &bytes);
    }
  }
}

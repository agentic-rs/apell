mod browse_cache;
mod download_pause;
mod restore;
mod state;
#[cfg(test)]
mod tests;
mod updates;

use crate::host::Host;
use apell_core::{
  bundles::{
    AppletListReport, InstalledApplet, PackageSource, StoreChannel, StoreSource,
    StoreSwitchSnapshot,
  },
  store_client::{
    normalize_store_url, DownloadCheckpoint, DownloadIdentity, DownloadOutcome, PendingDownload,
    StoreApplet, StoreCatalog, StoreClient, StoreDetail, StoreRelease,
  },
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

pub(crate) use browse_cache::BrowseCache;
pub(crate) use download_pause::DownloadPauses;
pub use state::StoreConfig;

const DOWNLOAD_MISSING: &str =
  "The saved download is no longer on this device. Review the release to start again.";
const DEFAULT_STORE_URL: Option<&str> = if cfg!(all(
  feature = "local-dev",
  target_os = "macos",
  debug_assertions
)) {
  Some("https://localhost:8788")
} else {
  None
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoreQuery {
  store_url: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DetailQuery {
  store_url: String,
  applet_id: String,
  #[serde(default)]
  installed_source: bool,
  pending_download: Option<DownloadIdentity>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedDetailQuery {
  store_url: String,
  applet_id: String,
  #[serde(default)]
  installed_source: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IconQuery {
  store_url: String,
  icon_url: String,
}

#[derive(Serialize)]
struct PendingStoreDownload {
  #[serde(flatten)]
  download: PendingDownload,
  #[serde(skip_serializing_if = "Option::is_none")]
  restore_snapshot: Option<StoreSwitchSnapshot>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallQuery {
  store_url: String,
  applet_id: String,
  #[serde(default)]
  installed_source: bool,
  #[serde(default)]
  resume_download: bool,
  release: StoreRelease,
  expected_installed_sha256: Option<String>,
  switch_snapshot: Option<StoreSwitchSnapshot>,
  channel_snapshot: Option<StoreSwitchSnapshot>,
  operation_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PauseDownloadQuery {
  operation_id: Uuid,
}

#[derive(Serialize)]
struct CatalogItem {
  #[serde(flatten)]
  applet: StoreApplet,
  installed: Option<InstalledApplet>,
  update_available: bool,
  source_conflict: bool,
  installation_unavailable: bool,
}
#[derive(Serialize)]
struct CatalogResult {
  store_url: String,
  applets: Vec<CatalogItem>,
}

#[derive(Serialize)]
struct SavedDetailResult {
  store_url: String,
  #[serde(flatten)]
  detail: StoreDetail,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReleaseAction {
  Install,
  Update,
  SwitchSource,
  SwitchChannel,
  Installed,
  Older,
  Conflict,
}
#[derive(Serialize)]
struct ReleaseOption {
  #[serde(flatten)]
  release: StoreRelease,
  action: ReleaseAction,
  #[serde(skip_serializing_if = "Option::is_none")]
  action_reason: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  downloaded_bytes: Option<u64>,
}
#[derive(Serialize)]
struct DetailResult {
  store_url: String,
  applet: StoreApplet,
  releases: Vec<ReleaseOption>,
  installed: Option<InstalledApplet>,
  restart_required: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  switch_snapshot: Option<StoreSwitchSnapshot>,
  #[serde(skip_serializing_if = "Option::is_none")]
  installed_channel: Option<StoreChannel>,
  #[serde(skip_serializing_if = "Option::is_none")]
  channel_snapshot: Option<StoreSwitchSnapshot>,
}

struct InstallReview<'a> {
  installed: Option<&'a InstalledApplet>,
  switch_snapshot: Option<StoreSwitchSnapshot>,
  installed_channel: Option<StoreChannel>,
  channel_snapshot: Option<StoreSwitchSnapshot>,
  blocked: Option<&'static str>,
}

fn download_checkpoint(
  app: &AppHandle,
  store_url: &str,
  release: &StoreRelease,
) -> Result<DownloadCheckpoint, String> {
  Ok(DownloadCheckpoint::new(
    &download_cache(app)?,
    store_url,
    release,
  ))
}

fn download_cache(app: &AppHandle) -> Result<PathBuf, String> {
  app
    .path()
    .app_cache_dir()
    .map(|cache| cache.join("store-downloads"))
    .map_err(|_| "Store download cache is unavailable.".into())
}

async fn require_pending_download(
  cache: &Path,
  identity: &DownloadIdentity,
) -> Result<PendingDownload, String> {
  DownloadCheckpoint::pending_download(cache, identity)
    .await?
    .ok_or_else(|| DOWNLOAD_MISSING.into())
}

async fn review_pending_download(
  app: &AppHandle,
  host: &Host,
  identity: &DownloadIdentity,
) -> Result<PendingDownload, String> {
  let cache = download_cache(app)?;
  let _gate = host.store_gate.lock().await;
  require_pending_download(&cache, identity).await
}

fn input<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, String> {
  serde_json::from_value(params).map_err(|_| "Invalid store request.".into())
}
fn output<T: Serialize>(result: T) -> Result<Value, String> {
  serde_json::to_value(result).map_err(|_| "Could not encode the store response.".into())
}

fn discard_request(mut params: Value) -> Result<(DownloadIdentity, bool), String> {
  let fields = params.as_object_mut().ok_or("Invalid store request.")?;
  let repair = match fields.remove("repair") {
    None => false,
    Some(Value::Bool(true)) => true,
    _ => return Err("Invalid store request.".into()),
  };
  Ok((input(params)?, repair))
}
async fn config(host: &Host) -> Result<StoreConfig, String> {
  let root = host.root.clone();
  tauri::async_runtime::spawn_blocking(move || state::load(&root))
    .await
    .map_err(|_| "Could not read store settings.")?
}
async fn configured(host: &Host, requested: &str) -> Result<String, String> {
  let store_url = normalize_store_url(requested)?;
  if config(host).await?.store_url.as_ref() != Some(&store_url) {
    return Err("The store address changed. Reopen the Store before continuing.".into());
  }
  Ok(store_url)
}
fn store_client(_host: &Host, store_url: &str) -> Result<StoreClient, String> {
  #[cfg(all(feature = "local-dev", target_os = "macos", debug_assertions))]
  if Some(store_url) == DEFAULT_STORE_URL {
    return StoreClient::with_http_builder(store_url, _host.account.local_http_builder()?);
  }
  StoreClient::new(store_url)
}
async fn save_config(host: &Host, requested: &str) -> Result<StoreConfig, String> {
  let config = StoreConfig {
    store_url: if requested.trim().is_empty() {
      None
    } else {
      Some(normalize_store_url(requested.trim())?)
    },
  };
  let _gate = host.store_gate.lock().await;
  let root = host.root.clone();
  tauri::async_runtime::spawn_blocking(move || {
    state::save(&root, &config)?;
    Ok::<_, String>(config)
  })
  .await
  .map_err(|_| "Could not save store settings.")?
}
async fn installed(host: &Host) -> Result<AppletListReport, String> {
  let bundles = host.bundles.clone();
  tauri::async_runtime::spawn_blocking(move || bundles.list_report())
    .await
    .map_err(|_| "Could not read installed applets.")?
}

fn authorize_installed_source(
  installed: Option<&InstalledApplet>,
  origin: &str,
  applet_id: &str,
) -> Result<(), String> {
  let matches_source = installed.is_some_and(|saved| {
    saved.manifest.applet_id == applet_id
      && matches!(
        saved.source.as_ref(),
        Some(PackageSource::Store(source))
          if source.store_url == origin && source.applet_id == applet_id
      )
  });
  if matches_source {
    Ok(())
  } else {
    Err("The installed applet no longer uses this Store. Reopen its details.".into())
  }
}

async fn installed_applet(host: &Host, applet_id: &str) -> Result<InstalledApplet, String> {
  let bundles = host.bundles.clone();
  let applet_id = applet_id.to_owned();
  crate::commands::blocking(move || bundles.installed(&applet_id)).await
}

fn catalog_result(
  store_url: String,
  catalog: StoreCatalog,
  installed: &AppletListReport,
  fresh: bool,
) -> CatalogResult {
  let applets = catalog
    .applets
    .into_iter()
    .map(|applet| {
      let review = review_install(installed, &store_url, &applet.applet_id);
      let installation_unavailable = review.blocked.is_some();
      let source_conflict = installation_unavailable || review.switch_snapshot.is_some();
      let update_available = fresh
        && !source_conflict
        && review
          .installed
          .is_some_and(|saved| has_update(&applet, saved));
      CatalogItem {
        applet,
        installed: review.installed.cloned(),
        update_available,
        source_conflict,
        installation_unavailable,
      }
    })
    .collect();
  CatalogResult { store_url, applets }
}

async fn installed_detail_origin(
  host: &Host,
  store_url: &str,
  applet_id: &str,
) -> Result<String, String> {
  let origin = normalize_store_url(store_url)?;
  let saved = installed_applet(host, applet_id).await?;
  authorize_installed_source(Some(&saved), &origin, applet_id)?;
  Ok(origin)
}

pub async fn dispatch(
  app: AppHandle,
  host: &Host,
  method: &str,
  params: Value,
) -> Result<Value, String> {
  match method {
    "store.config" => {
      let _gate = host.store_gate.lock().await;
      output(config(host).await?)
    }
    "store.set_url" => {
      let request: StoreQuery = input(params)?;
      output(save_config(host, &request.store_url).await?)
    }
    "store.catalog" => {
      let request: StoreQuery = input(params)?;
      let store_url = configured(host, &request.store_url).await?;
      let observation = host.store_browse_cache.observe(&store_url, None);
      let catalog = store_client(host, &store_url)?.catalog().await?;
      if let Some(observation) = observation {
        observation
          .remember(host, browse_cache::Metadata::Catalog(catalog.clone()))
          .await;
      }
      let installed = installed(host).await?;
      output(catalog_result(store_url, catalog, &installed, true))
    }
    "store.saved_catalog" => {
      let request: StoreQuery = input(params)?;
      let store_url = configured(host, &request.store_url).await?;
      let Some(catalog) = browse_cache::saved_catalog(host, &store_url).await else {
        return Ok(Value::Null);
      };
      let installed = installed(host).await?;
      configured(host, &store_url).await?;
      output(catalog_result(store_url, catalog, &installed, false))
    }
    "store.saved_detail" => {
      let request: SavedDetailQuery = input(params)?;
      let origin = || async {
        if request.installed_source {
          installed_detail_origin(host, &request.store_url, &request.applet_id).await
        } else {
          configured(host, &request.store_url).await
        }
      };
      let store_url = origin().await?;
      let Some(detail) = browse_cache::saved_detail(host, &store_url, &request.applet_id).await
      else {
        return Ok(Value::Null);
      };
      origin().await?;
      output(SavedDetailResult { store_url, detail })
    }
    "store.saved_updates" => output(updates::saved(host).await?),
    "store.updates" => output(updates::check(host).await?),
    "store.detail" => {
      let request: DetailQuery = input(params)?;
      let store_url = if let Some(identity) = &request.pending_download {
        let store_url = normalize_store_url(&request.store_url)?;
        if request.installed_source || !identity.matches_applet(&store_url, &request.applet_id) {
          return Err("The saved download belongs to a different Store or applet.".into());
        }
        review_pending_download(&app, host, identity).await?;
        store_url
      } else if request.installed_source {
        installed_detail_origin(host, &request.store_url, &request.applet_id).await?
      } else {
        configured(host, &request.store_url).await?
      };
      let observation = request
        .pending_download
        .is_none()
        .then(|| {
          host
            .store_browse_cache
            .observe(&store_url, Some(&request.applet_id))
        })
        .flatten();
      let detail = store_client(host, &store_url)?
        .detail(&request.applet_id)
        .await?;
      let installed = installed(host).await?;
      let review = review_install(&installed, &store_url, &request.applet_id);
      if request.installed_source {
        authorize_installed_source(review.installed, &store_url, &request.applet_id)?;
      }
      if let Some(observation) = observation {
        observation
          .remember(host, browse_cache::Metadata::Detail(detail.clone()))
          .await;
      }
      let pending = match &request.pending_download {
        Some(identity) => Some(review_pending_download(&app, host, identity).await?),
        None => None,
      };
      let mut releases = Vec::with_capacity(detail.releases.len());
      for release in detail.releases {
        let (action, action_reason) = release_action(&release, &review);
        let mut downloaded_bytes = None;
        if let Some(pending) = &pending {
          let Some(bytes) = pending.downloaded_bytes_for(&release) else {
            continue;
          };
          downloaded_bytes = Some(bytes);
        } else if matches!(
          action,
          ReleaseAction::Install
            | ReleaseAction::Update
            | ReleaseAction::SwitchSource
            | ReleaseAction::SwitchChannel
        ) {
          if let Ok(checkpoint) = download_checkpoint(&app, &store_url, &release) {
            let bytes = checkpoint.reviewed_received(release.size_bytes).await;
            downloaded_bytes = (bytes > 0).then_some(bytes);
          }
        }
        releases.push(ReleaseOption {
          release,
          action,
          action_reason,
          downloaded_bytes,
        });
      }
      output(DetailResult {
        store_url,
        applet: detail.applet,
        releases,
        installed: review.installed.cloned(),
        restart_required: review
          .installed
          .is_some_and(|saved| host.needs_applet_restart(saved)),
        switch_snapshot: review.switch_snapshot,
        installed_channel: review.installed_channel,
        channel_snapshot: review.channel_snapshot,
      })
    }
    "store.icon" => {
      let request: IconQuery = input(params)?;
      let store_url = configured(host, &request.store_url).await?;
      let _permit = host
        .store_icons
        .acquire()
        .await
        .map_err(|_| "Store icons are unavailable.")?;
      output(
        store_client(host, &store_url)?
          .icon(&request.icon_url)
          .await?,
      )
    }
    "store.pending_downloads" => {
      let downloads = {
        let _gate = host.store_gate.lock().await;
        DownloadCheckpoint::pending_downloads(&download_cache(&app)?).await?
      };
      let mut entries: Vec<_> = downloads
        .into_iter()
        .map(|download| PendingStoreDownload {
          download,
          restore_snapshot: None,
        })
        .collect();
      entries.extend(restore::pending_downloads(&app, host).await?);
      output(entries)
    }
    "store.discard_download" => {
      let (identity, repair) = discard_request(params)?;
      let discarded = if repair {
        restore::discard(&app, host, &identity).await
      } else {
        let cache = download_cache(&app)?;
        let _gate = host.store_gate.lock().await;
        DownloadCheckpoint::discard(&cache, &identity).await
      };
      publish_download_change(&app);
      output(discarded?)
    }
    "store.pause_download" => {
      let request: PauseDownloadQuery = input(params)?;
      output(host.store_download_pauses.request(request.operation_id)?)
    }
    "store.install" => {
      let result = install(app.clone(), host, input(params)?).await;
      publish_download_change(&app);
      result
    }
    "store.restore_package" => {
      let result = restore::restore_package(app.clone(), host, input(params)?).await;
      publish_download_change(&app);
      result
    }
    _ => Err("Unknown store command.".into()),
  }
}

pub(crate) fn publish_download_change(app: &AppHandle) {
  let _ = app.emit_to("main", "apell-store-download-changed", ());
}

fn review_install<'a>(
  report: &'a AppletListReport,
  store_url: &str,
  id: &str,
) -> InstallReview<'a> {
  let installed = report
    .applets
    .iter()
    .find(|saved| saved.manifest.applet_id == id);
  let blocked = report
    .unavailable
    .iter()
    .any(|saved| saved.applet_id == id)
    .then_some("An unavailable package already uses this applet identity. Its files and data were preserved.");
  let snapshot = installed
    .map(|saved| StoreSwitchSnapshot {
      sha256: saved.sha256.clone(),
      source: saved.source.clone(),
      removed: false,
    })
    .or_else(|| report.retained.get(id).cloned())
    .filter(|_| blocked.is_none());
  let installed_channel = match snapshot.as_ref().and_then(|state| state.source.as_ref()) {
    Some(PackageSource::Store(source))
      if source.store_url == store_url && source.applet_id == id =>
    {
      Some(source.channel)
    }
    _ => None,
  };
  let (channel_snapshot, switch_snapshot) = if installed_channel.is_some() {
    (snapshot, None)
  } else {
    (None, snapshot)
  };
  InstallReview {
    installed,
    switch_snapshot,
    installed_channel,
    channel_snapshot,
    blocked,
  }
}

fn installed_channel_release<'a>(
  applet: &'a StoreApplet,
  installed: &InstalledApplet,
) -> Option<&'a StoreRelease> {
  match installed.source.as_ref() {
    Some(PackageSource::Store(source)) => match source.channel {
      StoreChannel::Stable => applet.latest_stable_release.as_ref(),
      StoreChannel::Nightly => applet.nightly_release(),
    },
    _ => None,
  }
}

fn is_newer(release: &StoreRelease, installed: &InstalledApplet) -> bool {
  release
    .compare_version(&installed.manifest.version)
    .is_ok_and(|order| order.is_gt())
}

fn has_update(applet: &StoreApplet, installed: &InstalledApplet) -> bool {
  installed_channel_release(applet, installed).is_some_and(|release| is_newer(release, installed))
}

fn release_action(
  release: &StoreRelease,
  review: &InstallReview,
) -> (ReleaseAction, Option<String>) {
  if let Some(reason) = review.blocked {
    return (ReleaseAction::Conflict, Some(reason.into()));
  }
  let switching = review.switch_snapshot.is_some();
  let switching_channel = review
    .installed_channel
    .is_some_and(|current| current != StoreChannel::from_prerelease(release.prerelease));
  let Some(installed) = review.installed else {
    return (
      if switching {
        ReleaseAction::SwitchSource
      } else if switching_channel {
        ReleaseAction::SwitchChannel
      } else {
        ReleaseAction::Install
      },
      None,
    );
  };
  match release.compare_version(&installed.manifest.version) {
    Ok(order) if switching && !order.is_lt() => (ReleaseAction::SwitchSource, None),
    Ok(std::cmp::Ordering::Greater) if switching_channel => (ReleaseAction::SwitchChannel, None),
    Ok(std::cmp::Ordering::Greater) => (ReleaseAction::Update, None),
    Ok(std::cmp::Ordering::Less) => (
      ReleaseAction::Older,
      Some("A newer version is already installed.".into()),
    ),
    Ok(std::cmp::Ordering::Equal)
      if release.sha256 == installed.sha256 && release.version == installed.manifest.version =>
    {
      (ReleaseAction::Installed, None)
    }
    Err(_) => (
      ReleaseAction::Conflict,
      Some(
        "Install a versioned local package before switching this legacy applet to the Store."
          .into(),
      ),
    ),
    _ => (
      ReleaseAction::Conflict,
      Some(
        "This version does not match the installed package. Existing data was preserved.".into(),
      ),
    ),
  }
}

fn checked_switch(
  release: &StoreRelease,
  review: &InstallReview,
  expected_installed: Option<&str>,
  expected_switch: Option<&StoreSwitchSnapshot>,
  expected_channel: Option<&StoreSwitchSnapshot>,
) -> Result<ReleaseAction, String> {
  let (action, reason) = release_action(release, review);
  if matches!(action, ReleaseAction::Older | ReleaseAction::Conflict) {
    return Err(reason.unwrap_or_else(|| "This release cannot be installed.".into()));
  }
  if review.installed.map(|saved| saved.sha256.as_str()) != expected_installed {
    return Err("The installed applet changed. Review its details again.".into());
  }
  if action == ReleaseAction::SwitchChannel {
    if expected_switch.is_some() {
      return Err("The applet's installation source changed. Review its details again.".into());
    }
    return match expected_channel {
      Some(snapshot) if review.channel_snapshot.as_ref() == Some(snapshot) => Ok(action),
      Some(_) => Err("The installed applet changed. Review its details again.".into()),
      None => Err("Confirm the Store channel change before installing this release.".into()),
    };
  }
  if expected_channel.is_some() {
    return Err("The applet's Store channel changed. Review its details again.".into());
  }
  match (action, expected_switch) {
    (ReleaseAction::SwitchSource, Some(snapshot))
      if review.switch_snapshot.as_ref() == Some(snapshot) =>
    {
      Ok(action)
    }
    (ReleaseAction::SwitchSource, None) => {
      Err("Confirm the installation source change before installing this release.".into())
    }
    (ReleaseAction::SwitchSource, Some(_)) | (_, Some(_)) => {
      Err("The applet's installation source changed. Review its details again.".into())
    }
    _ => Ok(action),
  }
}

fn review_requested_install(
  request: &InstallQuery,
  report: &AppletListReport,
  store_url: &str,
) -> Result<ReleaseAction, String> {
  let review = review_install(report, store_url, &request.applet_id);
  if request.installed_source {
    authorize_installed_source(review.installed, store_url, &request.applet_id)?;
  }
  let action = checked_switch(
    &request.release,
    &review,
    request.expected_installed_sha256.as_deref(),
    request.switch_snapshot.as_ref(),
    request.channel_snapshot.as_ref(),
  )?;
  if request.installed_source
    && !matches!(action, ReleaseAction::Update | ReleaseAction::SwitchChannel)
  {
    return Err("Choose an update or confirm a different release channel for this applet.".into());
  }
  Ok(action)
}

async fn install(app: AppHandle, host: &Host, request: InstallQuery) -> Result<Value, String> {
  request.release.validate(&request.applet_id)?;
  let gate = host.store_gate.lock().await;
  let store_url = if request.installed_source || request.resume_download {
    normalize_store_url(&request.store_url)?
  } else {
    configured(host, &request.store_url).await?
  };
  let report = installed(host).await?;
  let action = review_requested_install(&request, &report, &store_url)?;
  let source = StoreSource {
    store_url: store_url.clone(),
    applet_id: request.applet_id.clone(),
    channel: StoreChannel::from_prerelease(request.release.prerelease),
  };
  let operation_id = request.operation_id;
  let cache = download_cache(&app)?;
  let checkpoint = DownloadCheckpoint::new(&cache, &store_url, &request.release);
  if request.resume_download {
    let identity = DownloadIdentity::for_release(&store_url, &request.applet_id, &request.release);
    let pending = require_pending_download(&cache, &identity).await?;
    if pending.downloaded_bytes_for(&request.release).is_none() {
      return Err("The saved download differs from this release. Review the release again.".into());
    }
  }
  let client = store_client(host, &store_url)?;
  let active = host.store_download_pauses.begin(&app, operation_id).await?;
  let download = client.download_resumable(
    &request.applet_id, &request.release, &checkpoint, active.wait_for_pause(), |received_bytes, total_bytes| {
      let _ = app.emit_to("main", "install-progress", json!({"operation_id":operation_id,"received_bytes":received_bytes,"total_bytes":total_bytes}));
    },
  ).await?;
  active.finish_transfer();
  let bytes = match download {
    DownloadOutcome::Verified(bytes) => bytes,
    DownloadOutcome::Paused { received_bytes } => {
      return Ok(json!({"status":"paused", "received_bytes":received_bytes}))
    }
  };
  let bundles = host.bundles.clone();
  let result = tauri::async_runtime::spawn_blocking(move || {
    if action == ReleaseAction::SwitchSource {
      let snapshot = request
        .switch_snapshot
        .as_ref()
        .ok_or("Missing installation source snapshot.")?;
      bundles.switch_to_store_release(&bytes, &request.release.sha256, source, snapshot)
    } else if action == ReleaseAction::SwitchChannel {
      let snapshot = request
        .channel_snapshot
        .as_ref()
        .ok_or("Missing Store channel snapshot.")?;
      bundles.switch_store_channel(&bytes, &request.release.sha256, source, snapshot)
    } else {
      bundles.install_store_release(
        &bytes,
        &request.release.sha256,
        source,
        request.expected_installed_sha256.as_deref(),
      )
    }
  })
  .await
  .map_err(|_| "Store installation was interrupted.")??;
  drop(gate);
  let mut result = crate::install::published_install_result(&app, host, result);
  result["status"] = json!("installed");
  Ok(result)
}

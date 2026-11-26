mod state;
#[cfg(test)]
mod tests;

use crate::host::Host;
use apell_core::{
  bundles::{
    AppletListReport, InstalledApplet, PackageSource, StoreChannel, StoreSource,
    StoreSwitchSnapshot,
  },
  store_client::{
    normalize_store_url, DownloadCheckpoint, DownloadOutcome, StoreApplet, StoreClient,
    StoreRelease,
  },
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

pub use state::StoreConfig;

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
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IconQuery {
  store_url: String,
  icon_url: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallQuery {
  store_url: String,
  applet_id: String,
  #[serde(default)]
  installed_source: bool,
  release: StoreRelease,
  expected_installed_sha256: Option<String>,
  switch_snapshot: Option<StoreSwitchSnapshot>,
  channel_snapshot: Option<StoreSwitchSnapshot>,
  operation_id: String,
}

#[derive(Serialize)]
struct CatalogItem {
  #[serde(flatten)]
  applet: StoreApplet,
  installed: Option<InstalledApplet>,
  update_available: bool,
  source_conflict: bool,
}
#[derive(Serialize)]
struct CatalogResult {
  store_url: String,
  applets: Vec<CatalogItem>,
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
  let cache = app
    .path()
    .app_cache_dir()
    .map_err(|_| "Store download cache is unavailable.")?;
  Ok(DownloadCheckpoint::new(
    &cache.join("store-downloads"),
    store_url,
    release,
  ))
}

fn input<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, String> {
  serde_json::from_value(params).map_err(|_| "Invalid store request.".into())
}
fn output<T: Serialize>(result: T) -> Result<Value, String> {
  serde_json::to_value(result).map_err(|_| "Could not encode the store response.".into())
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
      let catalog = store_client(host, &store_url)?.catalog().await?;
      let installed = installed(host).await?;
      let applets = catalog
        .applets
        .into_iter()
        .map(|applet| {
          let review = review_install(&installed, &store_url, &applet.applet_id);
          let source_conflict = review.blocked.is_some() || review.switch_snapshot.is_some();
          let update_available = !source_conflict
            && review
              .installed
              .is_some_and(|saved| has_update(&applet, saved));
          CatalogItem {
            applet,
            installed: review.installed.cloned(),
            update_available,
            source_conflict,
          }
        })
        .collect();
      output(CatalogResult { store_url, applets })
    }
    "store.detail" => {
      let request: DetailQuery = input(params)?;
      let store_url = if request.installed_source {
        let store_url = normalize_store_url(&request.store_url)?;
        let saved = installed_applet(host, &request.applet_id).await?;
        authorize_installed_source(Some(&saved), &store_url, &request.applet_id)?;
        store_url
      } else {
        configured(host, &request.store_url).await?
      };
      let detail = store_client(host, &store_url)?
        .detail(&request.applet_id)
        .await?;
      let installed = installed(host).await?;
      let review = review_install(&installed, &store_url, &request.applet_id);
      if request.installed_source {
        authorize_installed_source(review.installed, &store_url, &request.applet_id)?;
      }
      let mut releases = Vec::with_capacity(detail.releases.len());
      for release in detail.releases {
        let (action, action_reason) = release_action(&release, &review);
        let mut downloaded_bytes = None;
        if matches!(
          action,
          ReleaseAction::Install
            | ReleaseAction::Update
            | ReleaseAction::SwitchSource
            | ReleaseAction::SwitchChannel
        ) {
          if let Ok(checkpoint) = download_checkpoint(&app, &store_url, &release) {
            let bytes = checkpoint.received(release.size_bytes).await;
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
    "store.install" => install(app, host, input(params)?).await,
    _ => Err("Unknown store command.".into()),
  }
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

fn has_update(applet: &StoreApplet, installed: &InstalledApplet) -> bool {
  let candidate = match installed.source.as_ref() {
    Some(PackageSource::Store(source)) => match source.channel {
      StoreChannel::Stable => applet.latest_stable_release.as_ref(),
      StoreChannel::Nightly => applet.nightly_release(),
    },
    _ => None,
  };
  candidate.is_some_and(|candidate| {
    candidate
      .compare_version(&installed.manifest.version)
      .is_ok_and(|order| order.is_gt())
  })
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
  uuid::Uuid::parse_str(&request.operation_id)
    .map_err(|_| "Invalid install operation identity.")?;
  request.release.validate(&request.applet_id)?;
  let _gate = host.store_gate.lock().await;
  let store_url = if request.installed_source {
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
  let operation_id = &request.operation_id;
  let checkpoint = download_checkpoint(&app, &store_url, &request.release)?;
  #[cfg(target_os = "ios")]
  let lease = crate::ios::WakeLease::begin(&app).await;
  let pause = async {
    #[cfg(target_os = "ios")]
    if let Some(lease) = &lease {
      return lease.expired().await;
    }
    std::future::pending::<()>().await;
  };
  let download = store_client(host, &store_url)?.download_resumable(
    &request.applet_id, &request.release, &checkpoint, pause, |received_bytes, total_bytes| {
      let _ = app.emit_to("main", "install-progress", json!({"operation_id":operation_id,"received_bytes":received_bytes,"total_bytes":total_bytes}));
    },
  ).await?;
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
  let restart_required = host.needs_applet_restart(&result);
  Ok(json!({"status":"installed", "applet":result, "restart_required":restart_required}))
}

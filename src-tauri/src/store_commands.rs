mod state;
#[cfg(test)]
mod tests;

use crate::host::Host;
use apell_core::{
  bundles::{AppletListReport, InstalledApplet, PackageSource, StoreChannel, StoreSource},
  store_client::{normalize_store_url, StoreApplet, StoreClient, StoreRelease},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

pub use state::StoreConfig;

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
  release: StoreRelease,
  expected_installed_sha256: Option<String>,
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
}
#[derive(Serialize)]
struct DetailResult {
  store_url: String,
  applet: StoreApplet,
  releases: Vec<ReleaseOption>,
  installed: Option<InstalledApplet>,
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
      let catalog = StoreClient::new(&store_url)?.catalog().await?;
      let installed = installed(host).await?;
      let applets = catalog
        .applets
        .into_iter()
        .map(|applet| {
          let saved = installed
            .applets
            .iter()
            .find(|saved| saved.manifest.applet_id == applet.applet_id)
            .cloned();
          let source_conflict = blocked_install_reason(&installed, &store_url, &applet.applet_id)
            .is_some()
            || saved
              .as_ref()
              .is_some_and(|saved| !same_store(saved, &store_url, &applet.applet_id));
          let update_available = !source_conflict
            && saved
              .as_ref()
              .is_some_and(|saved| has_update(&applet, saved));
          CatalogItem {
            applet,
            installed: saved,
            update_available,
            source_conflict,
          }
        })
        .collect();
      output(CatalogResult { store_url, applets })
    }
    "store.detail" => {
      let request: DetailQuery = input(params)?;
      let store_url = configured(host, &request.store_url).await?;
      let detail = StoreClient::new(&store_url)?
        .detail(&request.applet_id)
        .await?;
      let installed = installed(host).await?;
      let blocked = blocked_install_reason(&installed, &store_url, &request.applet_id);
      let saved = installed
        .applets
        .into_iter()
        .find(|saved| saved.manifest.applet_id == request.applet_id);
      let releases = detail
        .releases
        .into_iter()
        .map(|release| {
          let (action, action_reason) = release_action(
            &store_url,
            &request.applet_id,
            &release,
            saved.as_ref(),
            blocked,
          );
          ReleaseOption {
            release,
            action,
            action_reason,
          }
        })
        .collect();
      output(DetailResult {
        store_url,
        applet: detail.applet,
        releases,
        installed: saved,
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
        StoreClient::new(&store_url)?
          .icon(&request.icon_url)
          .await?,
      )
    }
    "store.install" => install(app, host, input(params)?).await,
    _ => Err("Unknown store command.".into()),
  }
}

fn same_store(installed: &InstalledApplet, store_url: &str, applet_id: &str) -> bool {
  is_store_source(installed.source.as_ref(), store_url, applet_id)
}

fn is_store_source(source: Option<&PackageSource>, store_url: &str, applet_id: &str) -> bool {
  matches!(source, Some(PackageSource::Store(source)) if source.store_url == store_url && source.applet_id == applet_id)
}

fn blocked_install_reason(
  report: &AppletListReport,
  store_url: &str,
  id: &str,
) -> Option<&'static str> {
  if report.unavailable.iter().any(|saved| saved.applet_id == id) {
    return Some("An unavailable package already uses this applet identity. Its files and data were preserved.");
  }
  report
    .retained_sources
    .get(id)
    .filter(|source| !is_store_source(source.as_ref(), store_url, id))
    .map(|_| {
      "This applet has saved data from a different source. Reinstall it from its original source."
    })
}

fn has_update(applet: &StoreApplet, installed: &InstalledApplet) -> bool {
  let candidate = match installed.source.as_ref() {
    Some(PackageSource::Store(source)) => match source.channel {
      StoreChannel::Stable => applet.latest_stable_release.as_ref(),
      StoreChannel::Preview => Some(&applet.latest_release),
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
  store_url: &str,
  applet_id: &str,
  release: &StoreRelease,
  installed: Option<&InstalledApplet>,
  blocked: Option<&str>,
) -> (ReleaseAction, Option<String>) {
  if let Some(reason) = blocked {
    return (ReleaseAction::Conflict, Some(reason.into()));
  }
  let Some(installed) = installed else {
    return (ReleaseAction::Install, None);
  };
  if !same_store(installed, store_url, applet_id) {
    return (ReleaseAction::Conflict, Some("This applet was installed from a file, GitHub, or another store. Update it from its original source.".into()));
  }
  match release.compare_version(&installed.manifest.version) {
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
    _ => (
      ReleaseAction::Conflict,
      Some(
        "This version does not match the installed package. Existing data was preserved.".into(),
      ),
    ),
  }
}

async fn install(app: AppHandle, host: &Host, request: InstallQuery) -> Result<Value, String> {
  uuid::Uuid::parse_str(&request.operation_id)
    .map_err(|_| "Invalid install operation identity.")?;
  request.release.validate(&request.applet_id)?;
  let _gate = host.store_gate.lock().await;
  let store_url = configured(host, &request.store_url).await?;
  let report = installed(host).await?;
  let saved = report
    .applets
    .iter()
    .find(|saved| saved.manifest.applet_id == request.applet_id);
  let (action, reason) = release_action(
    &store_url,
    &request.applet_id,
    &request.release,
    saved,
    blocked_install_reason(&report, &store_url, &request.applet_id),
  );
  if !matches!(
    action,
    ReleaseAction::Install | ReleaseAction::Update | ReleaseAction::Installed
  ) {
    return Err(reason.unwrap_or_else(|| "This release cannot be installed.".into()));
  }
  if saved.map(|saved| saved.sha256.as_str()) != request.expected_installed_sha256.as_deref() {
    return Err("The installed applet changed. Review its details again.".into());
  }
  let source = StoreSource {
    store_url: store_url.clone(),
    applet_id: request.applet_id.clone(),
    channel: if request.release.prerelease {
      StoreChannel::Preview
    } else {
      StoreChannel::Stable
    },
  };
  let operation_id = request.operation_id;
  let applet_id = request.applet_id;
  let bytes = StoreClient::new(&store_url)?.download(&applet_id, &request.release, move |received_bytes, total_bytes| {
    let _ = app.emit_to("main", "install-progress", json!({"operation_id":operation_id,"received_bytes":received_bytes,"total_bytes":total_bytes}));
  }).await?;
  let bundles = host.bundles.clone();
  let result = tauri::async_runtime::spawn_blocking(move || {
    bundles.install_store_release(
      &bytes,
      &request.release.sha256,
      source,
      request.expected_installed_sha256.as_deref(),
    )
  })
  .await
  .map_err(|_| "Store installation was interrupted.")??;
  output(result)
}

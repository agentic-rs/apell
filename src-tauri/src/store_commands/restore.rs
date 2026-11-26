use super::{store_client, PendingStoreDownload, DOWNLOAD_MISSING};
use crate::{commands::blocking, host::Host, install::install_result};
use apell_core::{
  bundles::{
    AppletReportEntry, BundleStore, PackageSource, StoreChannel, StoreSource, StoreSwitchSnapshot,
  },
  store_client::{
    DownloadCheckpoint, DownloadIdentity, DownloadOutcome, PendingDownload, StoreRelease,
  },
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;

const INSTALLATION_CHANGED: &str =
  "The installed applet changed. Refresh the library before restoring.";
const RELEASE_MISSING: &str =
  "The original release is no longer available from this Store. Choose the original .apel file.";
const CHECKPOINT_CHANGED: &str =
  "The saved repair differs from the original release. Start the repair again from Library.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RestoreQuery {
  applet_id: String,
  expected: StoreSwitchSnapshot,
  operation_id: Uuid,
  pending_download: Option<DownloadIdentity>,
}

fn restore_snapshot(
  store: &BundleStore,
  applet_id: &str,
) -> Result<Option<StoreSwitchSnapshot>, String> {
  Ok(match store.report_applet(applet_id)? {
    Some(AppletReportEntry::Unavailable(applet)) => applet.restore_snapshot,
    _ => None,
  })
}

fn restore_source(
  store: &BundleStore,
  applet_id: &str,
  expected: &StoreSwitchSnapshot,
) -> Result<StoreSource, String> {
  let snapshot = restore_snapshot(store, applet_id)?
    .filter(|snapshot| snapshot == expected)
    .ok_or(INSTALLATION_CHANGED)?;
  match snapshot.source {
    Some(PackageSource::Store(source)) => Ok(source),
    _ => Err("Choose the original .apel file for this applet.".into()),
  }
}

async fn checked_source(host: &Host, request: &RestoreQuery) -> Result<StoreSource, String> {
  let bundles = host.bundles.clone();
  let applet_id = request.applet_id.clone();
  let expected = request.expected.clone();
  let _gate = host.store_gate.lock().await;
  blocking(move || restore_source(&bundles, &applet_id, &expected)).await
}

fn repair_cache(app: &AppHandle) -> Result<PathBuf, String> {
  app
    .path()
    .app_cache_dir()
    .map(|cache| cache.join("store-repairs"))
    .map_err(|_| "Store repair cache is unavailable.".into())
}

fn download_snapshot(identity: &DownloadIdentity) -> StoreSwitchSnapshot {
  StoreSwitchSnapshot {
    sha256: identity.sha256().into(),
    source: Some(identity.store_source().into()),
    removed: false,
  }
}

fn partition_downloads(
  bundles: &BundleStore,
  pending: Vec<PendingDownload>,
) -> Result<(Vec<PendingStoreDownload>, Vec<PendingDownload>), String> {
  let mut current = Vec::new();
  let mut stale = Vec::new();
  for download in pending {
    let identity = download.identity();
    let expected = download_snapshot(identity);
    if restore_snapshot(bundles, &identity.store_source().applet_id)?.as_ref() == Some(&expected) {
      current.push(PendingStoreDownload {
        download,
        restore_snapshot: Some(expected),
      });
    } else {
      stale.push(download);
    }
  }
  Ok((current, stale))
}

// The caller holds store_repairs_gate while reconciling or consuming saved rows.
async fn reconcile_repairs(cache: &Path, host: &Host) -> Result<Vec<PendingStoreDownload>, String> {
  let pending = DownloadCheckpoint::pending_downloads(cache).await?;
  if pending.is_empty() {
    return Ok(Vec::new());
  }
  let _store_gate = host.store_gate.lock().await;
  let bundles = host.bundles.clone();
  let (current, stale) = blocking(move || partition_downloads(&bundles, pending)).await?;
  for download in stale {
    DownloadCheckpoint::discard(cache, download.identity()).await?;
  }
  Ok(current)
}

pub(super) async fn pending_downloads(
  app: &AppHandle,
  host: &Host,
) -> Result<Vec<PendingStoreDownload>, String> {
  let cache = repair_cache(app)?;
  let _repairs_gate = host.store_repairs_gate.lock().await;
  reconcile_repairs(&cache, host).await
}

pub(super) async fn discard(
  app: &AppHandle,
  host: &Host,
  identity: &DownloadIdentity,
) -> Result<bool, String> {
  let cache = repair_cache(app)?;
  let _repairs_gate = host.store_repairs_gate.lock().await;
  DownloadCheckpoint::discard(&cache, identity).await
}

fn requested_checkpoint(
  repairs: Vec<PendingStoreDownload>,
  request: &RestoreQuery,
) -> Result<Option<PendingDownload>, String> {
  let Some(identity) = request.pending_download.as_ref() else {
    return Ok(None);
  };
  if download_snapshot(identity) != request.expected {
    return Err(CHECKPOINT_CHANGED.into());
  }
  repairs
    .into_iter()
    .map(|entry| entry.download)
    .find(|download| download.identity() == identity)
    .map(Some)
    .ok_or_else(|| DOWNLOAD_MISSING.into())
}

async fn paused(
  host: &Host,
  request: &RestoreQuery,
  cache: &Path,
  identity: Option<&DownloadIdentity>,
  received_bytes: u64,
) -> Result<Value, String> {
  let _gate = host.store_gate.lock().await;
  let bundles = host.bundles.clone();
  let applet_id = request.applet_id.clone();
  let current = blocking(move || restore_snapshot(&bundles, &applet_id)).await?;
  if current.as_ref() != Some(&request.expected) {
    if let Some(identity) = identity {
      DownloadCheckpoint::discard(cache, identity).await?;
    }
    return Err(INSTALLATION_CHANGED.into());
  }
  Ok(json!({"status":"paused", "received_bytes":received_bytes}))
}

fn exact_release<'a>(
  releases: &'a [StoreRelease],
  sha256: &str,
  channel: StoreChannel,
) -> Result<&'a StoreRelease, String> {
  let mut matching = releases.iter().filter(|release| {
    release.sha256 == sha256 && StoreChannel::from_prerelease(release.prerelease) == channel
  });
  let release = matching.next().ok_or(RELEASE_MISSING)?;
  if matching.next().is_some() {
    return Err("The Store returned ambiguous releases. Choose the original .apel file.".into());
  }
  Ok(release)
}

pub(super) async fn restore_package(
  app: AppHandle,
  host: &Host,
  request: RestoreQuery,
) -> Result<Value, String> {
  // The repair cache is independent from ordinary installs. Acquire its gate
  // before taking short-lived Store gates for local authorization/publication.
  let cache = repair_cache(&app)?;
  let _repairs_gate = host.store_repairs_gate.lock().await;
  let source = checked_source(host, &request).await?;
  let pending = requested_checkpoint(reconcile_repairs(&cache, host).await?, &request)?;
  let client = store_client(host, &source.store_url)?;
  #[cfg(target_os = "ios")]
  let lease = crate::ios::WakeLease::begin(&app).await;
  let pause = async {
    #[cfg(target_os = "ios")]
    if let Some(lease) = &lease {
      return lease.expired().await;
    }
    std::future::pending::<()>().await;
  };
  tokio::pin!(pause);
  let detail = tokio::select! {
    biased;
    _ = &mut pause => {
      return paused(host, &request, &cache, request.pending_download.as_ref(),
        pending.as_ref().map_or(0, PendingDownload::downloaded_bytes)).await;
    }
    result = client.detail(&request.applet_id) => result?,
  };
  let release = exact_release(&detail.releases, &request.expected.sha256, source.channel)?;
  if pending
    .as_ref()
    .is_some_and(|pending| pending.downloaded_bytes_for(release).is_none())
  {
    return Err(CHECKPOINT_CHANGED.into());
  }
  let identity = DownloadIdentity::for_release(&source.store_url, &request.applet_id, release);
  let checkpoint = DownloadCheckpoint::new(&cache, &source.store_url, release);
  let outcome = client.download_resumable_with_recheck(
    &request.applet_id, release, &checkpoint, &mut pause,
    async { checked_source(host, &request).await.map(|_| ()) },
    |received_bytes, total_bytes| {
      let _ = app.emit_to("main", "install-progress", json!({
        "operation_id":request.operation_id,"received_bytes":received_bytes,"total_bytes":total_bytes,
      }));
    },
  ).await?;
  let bytes = match outcome {
    DownloadOutcome::Verified(bytes) => bytes,
    DownloadOutcome::Paused { received_bytes } => {
      return paused(host, &request, &cache, Some(&identity), received_bytes).await;
    }
  };

  let _gate = host.store_gate.lock().await;
  let bundles = host.bundles.clone();
  let installed = blocking(move || {
    restore_source(&bundles, &request.applet_id, &request.expected)?;
    bundles.restore_package(&request.applet_id, &request.expected, &bytes)
  })
  .await?;
  let mut result = install_result(host, installed);
  result["status"] = json!("restored");
  Ok(result)
}

#[cfg(test)]
mod tests {
  use super::{super::tests::listing, *};
  use apell_core::bundles::digest;
  use std::fs;

  fn source() -> StoreSource {
    StoreSource {
      store_url: "https://original.example.invalid".into(),
      applet_id: "sample".into(),
      channel: StoreChannel::Stable,
    }
  }

  fn snapshot(source: Option<PackageSource>) -> StoreSwitchSnapshot {
    StoreSwitchSnapshot {
      sha256: "a".repeat(64),
      source,
      removed: false,
    }
  }

  fn write_active(root: &Path, snapshot: &StoreSwitchSnapshot) {
    let directory = root.join("sample");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
      directory.join("active"),
      json!({
        "applet_id":"sample", "sha256":snapshot.sha256, "source":snapshot.source,
        "removed":snapshot.removed,
      })
      .to_string(),
    )
    .unwrap();
  }

  fn save_checkpoint(
    cache: &Path,
    source: &StoreSource,
    release: &StoreRelease,
  ) -> DownloadIdentity {
    let identity = DownloadIdentity::for_release(&source.store_url, &source.applet_id, release);
    let key = digest(
      format!(
        "{}\n{}\n{}",
        source.store_url, release.download_url, release.sha256
      )
      .as_bytes(),
    );
    fs::create_dir_all(cache).unwrap();
    fs::write(cache.join(format!("{key}.part")), b"partial").unwrap();
    fs::write(
      cache.join(format!("{key}.json")),
      json!({
        "identity":identity, "name":"Sample", "size_bytes":release.size_bytes,
      })
      .to_string(),
    )
    .unwrap();
    identity
  }

  #[test]
  fn restore_request_accepts_only_the_reviewed_snapshot_and_operation_id() {
    let valid = json!({
      "applet_id":"sample", "expected":snapshot(Some(source().into())), "operation_id":Uuid::new_v4(),
    });
    assert!(serde_json::from_value::<RestoreQuery>(valid.clone()).is_ok());

    let mut invalid = valid.clone();
    invalid["operation_id"] = json!("wrong-operation");
    assert!(serde_json::from_value::<RestoreQuery>(invalid).is_err());
    let mut invalid = valid.clone();
    invalid["store_url"] = json!("https://other.example.invalid");
    assert!(serde_json::from_value::<RestoreQuery>(invalid).is_err());
    let mut invalid = valid;
    invalid["expected"]
      .as_object_mut()
      .unwrap()
      .remove("source");
    assert!(serde_json::from_value::<RestoreQuery>(invalid).is_err());
  }

  #[test]
  fn restore_source_requires_the_exact_current_repairable_store_record() {
    let root = tempfile::tempdir().unwrap();
    let store = BundleStore::new(root.path().into());
    let directory = root.path().join("sample");
    let original = source();
    let expected = snapshot(Some(original.clone().into()));
    write_active(root.path(), &expected);
    assert_eq!(
      restore_source(&store, "sample", &expected).unwrap(),
      original
    );
    assert_eq!(
      restore_source(&store, "another", &expected).unwrap_err(),
      INSTALLATION_CHANGED
    );

    let mut changed_digest = expected.clone();
    changed_digest.sha256 = "b".repeat(64);
    let forged_store = snapshot(Some(
      StoreSource {
        store_url: "https://other.example.invalid".into(),
        ..original.clone()
      }
      .into(),
    ));
    let changed_track = snapshot(Some(
      StoreSource {
        channel: StoreChannel::Nightly,
        ..original
      }
      .into(),
    ));
    for stale in [changed_digest, forged_store, changed_track] {
      assert_eq!(
        restore_source(&store, "sample", &stale).unwrap_err(),
        INSTALLATION_CHANGED
      );
    }

    let archive = directory.join(format!("{}.apel", expected.sha256));
    fs::create_dir(&archive).unwrap();
    assert_eq!(
      restore_source(&store, "sample", &expected).unwrap_err(),
      INSTALLATION_CHANGED
    );
    fs::remove_dir(archive).unwrap();
    store.remove("sample", || Ok(())).unwrap();
    assert_eq!(
      restore_source(&store, "sample", &expected).unwrap_err(),
      INSTALLATION_CHANGED
    );

    let file = snapshot(None);
    write_active(root.path(), &file);
    assert_eq!(
      restore_source(&store, "sample", &file).unwrap_err(),
      "Choose the original .apel file for this applet."
    );
  }

  #[tokio::test]
  async fn repair_checkpoints_are_separate_and_pruned_against_the_current_installation() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::new(root.path().into()).unwrap();
    let repairs = root.path().join("store-repairs");
    let installs = root.path().join("store-downloads");
    let source = source();
    let release = listing("sample", "1.0.0", None).latest_release;
    let expected = snapshot(Some(source.clone().into()));
    write_active(&root.path().join("bundles"), &expected);
    let same_install = save_checkpoint(&installs, &source, &release);
    let mut request = RestoreQuery {
      applet_id: "sample".into(),
      expected: expected.clone(),
      operation_id: Uuid::new_v4(),
      pending_download: Some(same_install),
    };
    let _repairs_gate = host.store_repairs_gate.lock().await;
    let current = || reconcile_repairs(&repairs, &host);
    assert_eq!(
      requested_checkpoint(current().await.unwrap(), &request).unwrap_err(),
      DOWNLOAD_MISSING
    );

    let other = StoreSource {
      store_url: "https://other.example.invalid".into(),
      ..source.clone()
    };
    let foreign = save_checkpoint(&repairs, &other, &release);
    request.pending_download = Some(foreign);
    assert_eq!(
      requested_checkpoint(current().await.unwrap(), &request).unwrap_err(),
      CHECKPOINT_CHANGED
    );
    let identity = save_checkpoint(&repairs, &source, &release);
    request.pending_download = Some(identity);
    let pending = requested_checkpoint(current().await.unwrap(), &request)
      .unwrap()
      .unwrap();
    assert_eq!(pending.downloaded_bytes_for(&release), Some(7));
    let different_size = StoreRelease {
      size_bytes: release.size_bytes + 1,
      ..release
    };
    assert_eq!(pending.downloaded_bytes_for(&different_size), None);

    let rows = current().await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = serde_json::to_value(&rows[0]).unwrap();
    assert_eq!(row["restore_snapshot"], json!(expected));
    assert_eq!(row["origin"], source.store_url);
    assert_eq!(row["downloaded_bytes"], 7);
    assert_eq!(
      DownloadCheckpoint::pending_downloads(&repairs)
        .await
        .unwrap()
        .len(),
      1
    );
    let identity = request.pending_download.as_ref();
    assert_eq!(
      paused(&host, &request, &repairs, identity, 7)
        .await
        .unwrap(),
      json!({ "status":"paused", "received_bytes":7 })
    );

    host.bundles.remove("sample", || Ok(())).unwrap();
    assert_eq!(
      paused(&host, &request, &repairs, identity, 7)
        .await
        .unwrap_err(),
      INSTALLATION_CHANGED
    );
    assert!(DownloadCheckpoint::pending_downloads(&repairs)
      .await
      .unwrap()
      .is_empty());
    save_checkpoint(&repairs, &source, &different_size);
    assert!(current().await.unwrap().is_empty());
    assert!(DownloadCheckpoint::pending_downloads(&repairs)
      .await
      .unwrap()
      .is_empty());
    assert_eq!(
      DownloadCheckpoint::pending_downloads(&installs)
        .await
        .unwrap()
        .len(),
      1
    );
  }

  #[test]
  fn repair_discard_only_accepts_an_explicit_true_selector() {
    let origin = source();
    let release = listing("sample", "1.0.0", None).latest_release;
    let identity = DownloadIdentity::for_release(&origin.store_url, &origin.applet_id, &release);
    let mut params = json!(identity);
    assert_eq!(
      super::super::discard_request(params.clone()).unwrap(),
      (identity, false)
    );
    for invalid in [json!(false), json!("true"), Value::Null] {
      params["repair"] = invalid;
      assert!(super::super::discard_request(params.clone()).is_err());
    }
    params["repair"] = json!(true);
    let (requested, repair) = super::super::discard_request(params).unwrap();
    assert!(repair);
    assert_eq!(download_snapshot(&requested), snapshot(Some(origin.into())));
  }

  #[test]
  fn restore_selects_the_unique_matching_digest_from_the_recorded_channel() {
    let applet = listing("sample", "2.0.0", Some("2.1.0-nightly.1"));
    let mut head = applet.latest_stable_release.unwrap();
    let mut original = head.clone();
    original.version = "1.0.0".into();
    original.download_url = "/api/v1/applets/sample/versions/1.0.0/download".into();
    head.sha256 = "b".repeat(64);
    let nightly = applet.latest_nightly_release.unwrap();
    let releases = [head.clone(), nightly, original.clone()];
    assert_eq!(
      exact_release(&releases, &original.sha256, StoreChannel::Stable).unwrap(),
      &original
    );
    assert_eq!(
      exact_release(&releases, &head.sha256, StoreChannel::Nightly).unwrap_err(),
      RELEASE_MISSING
    );
    assert_eq!(
      exact_release(&releases, &"f".repeat(64), StoreChannel::Stable).unwrap_err(),
      RELEASE_MISSING
    );

    head.sha256.clone_from(&original.sha256);
    let ambiguous = [head, original.clone()];
    assert!(
      exact_release(&ambiguous, &original.sha256, StoreChannel::Stable)
        .unwrap_err()
        .contains("ambiguous")
    );
  }
}

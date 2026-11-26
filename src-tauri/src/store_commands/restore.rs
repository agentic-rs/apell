use super::store_client;
use crate::{commands::blocking, host::Host, install::install_result};
use apell_core::{
  bundles::{
    AppletReportEntry, BundleStore, PackageSource, StoreChannel, StoreSource, StoreSwitchSnapshot,
  },
  store_client::StoreRelease,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use uuid::Uuid;

const INSTALLATION_CHANGED: &str =
  "The installed applet changed. Refresh the library before restoring.";
const RELEASE_MISSING: &str =
  "The original release is no longer available from this Store. Choose the original .apel file.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RestoreQuery {
  applet_id: String,
  expected: StoreSwitchSnapshot,
  operation_id: Uuid,
}

fn restore_source(
  store: &BundleStore,
  applet_id: &str,
  expected: &StoreSwitchSnapshot,
) -> Result<StoreSource, String> {
  let Some(AppletReportEntry::Unavailable(applet)) = store.report_applet(applet_id)? else {
    return Err(INSTALLATION_CHANGED.into());
  };
  let snapshot = applet
    .restore_snapshot
    .filter(|snapshot| snapshot == expected)
    .ok_or(INSTALLATION_CHANGED)?;
  match snapshot.source {
    Some(PackageSource::Store(source)) => Ok(source),
    _ => Err("Choose the original .apel file for this applet.".into()),
  }
}

async fn checked_source(host: &Host, request: &RestoreQuery) -> Result<StoreSource, String> {
  let _gate = host.store_gate.lock().await;
  let bundles = host.bundles.clone();
  let applet_id = request.applet_id.clone();
  let expected = request.expected.clone();
  blocking(move || restore_source(&bundles, &applet_id, &expected)).await
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
  let source = checked_source(host, &request).await?;
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
  let download = async {
    let detail = client.detail(&request.applet_id).await?;
    let release = exact_release(&detail.releases, &request.expected.sha256, source.channel)?;
    // Release lookup may take a while. Check the local record again before
    // asking the Store for the package body.
    checked_source(host, &request).await?;
    client
      .download(&request.applet_id, release, |received_bytes, total_bytes| {
        let _ = app.emit_to("main", "install-progress", json!({
          "operation_id":request.operation_id,"received_bytes":received_bytes,"total_bytes":total_bytes,
        }));
      })
      .await
  };
  let bytes = tokio::select! {
    biased;
    _ = pause => return Err("The Store restore was interrupted. Try restoring again.".into()),
    result = download => result?,
  };

  let _gate = host.store_gate.lock().await;
  let bundles = host.bundles.clone();
  let installed =
    blocking(move || bundles.restore_package(&request.applet_id, &request.expected, &bytes))
      .await?;
  Ok(install_result(host, installed))
}

#[cfg(test)]
mod tests {
  use super::{super::tests::listing, *};
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
    fs::create_dir(&directory).unwrap();
    let pointer = directory.join("active");
    let original = source();
    let expected = snapshot(Some(original.clone().into()));
    let write_record = |snapshot: &StoreSwitchSnapshot| {
      fs::write(
        &pointer,
        json!({
          "applet_id":"sample", "sha256":snapshot.sha256, "source":snapshot.source,
          "removed":snapshot.removed,
        })
        .to_string(),
      )
      .unwrap();
    };
    write_record(&expected);
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
    write_record(&file);
    assert_eq!(
      restore_source(&store, "sample", &file).unwrap_err(),
      "Choose the original .apel file for this applet."
    );
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

use super::*;
use std::fs;

#[test]
fn restoring_the_same_package_invalidates_an_unavailable_removal_review() {
  let dir = tempfile::tempdir().unwrap();
  let store = BundleStore::new(dir.path().into());
  let bytes = package("sample", "1.0.0");
  let installed = store.install(&bytes, &digest(&bytes)).unwrap();
  let archive = dir
    .path()
    .join("sample")
    .join(format!("{}.apel", installed.sha256));
  fs::remove_file(archive).unwrap();

  let unavailable = store.list_report().unwrap().unavailable.pop().unwrap();
  let reviewed = unavailable.removal_revision.unwrap();
  assert_ne!(reviewed, installed.removal_revision);
  assert!(store
    .prepare_removal("sample", &reviewed)
    .unwrap()
    .is_some());

  let restored = store
    .restore_package("sample", &unavailable.restore_snapshot.unwrap(), &bytes)
    .unwrap();
  assert_eq!(restored.removal_revision, installed.removal_revision);
  assert!(store
    .prepare_removal("sample", &reviewed)
    .unwrap()
    .is_none());
  store
    .prepare_removal("sample", &restored.removal_revision)
    .unwrap()
    .unwrap()
    .remove(|| Ok(()))
    .unwrap();
  assert!(store.installed("sample").is_err());
}

#[test]
fn removal_keeps_only_the_binding_and_accepts_a_fresh_install_from_its_source() {
  let github: PackageSource = source(10, "sample.apel").into();
  let stable: PackageSource = store_source(StoreChannel::Stable).into();
  let sources = [None, Some(github.clone()), Some(stable.clone())];
  for original in &sources {
    let dir = tempfile::tempdir().unwrap();
    let store = BundleStore::new(dir.path().into());
    let first = package("sample", "1.0.0");
    let next = package("sample", "2.0.0");
    store
      .install_with_source(&first, None, original.clone())
      .unwrap();
    store
      .install_with_source(&next, None, original.clone())
      .unwrap();
    let other = package("other", "1.0.0");
    store.install(&other, &digest(&other)).unwrap();

    store.remove("sample", || Ok(())).unwrap();
    let reopened = BundleStore::new(dir.path().into());
    reopened
      .remove("sample", || panic!("already removed"))
      .unwrap();
    assert!(reopened.load("sample").is_err());
    assert!(reopened.load_with_metadata("sample").is_err());
    assert!(reopened.installed("sample").is_err());
    assert_eq!(reopened.list().unwrap().len(), 1);
    let report = reopened.list_report().unwrap();
    assert!(report.unavailable.is_empty());
    assert_eq!(report.applets[0].manifest.applet_id, "other");
    assert_eq!(
      report.retained.get("sample"),
      Some(&StoreSwitchSnapshot {
        sha256: digest(&next),
        source: original.clone(),
        removed: true,
      })
    );
    assert!(serde_json::to_value(report)
      .unwrap()
      .get("retained")
      .is_none());
    assert_eq!(fs::read_dir(dir.path().join("sample")).unwrap().count(), 1);

    for foreign in sources.iter().filter(|incoming| *incoming != original) {
      assert!(reopened
        .install_with_source(&next, None, foreign.clone())
        .is_err());
    }
    assert!(reopened
      .update_from_source("sample", &next, None, github.clone())
      .is_err());
    let hash = digest(&next);
    let nightly = store_source(StoreChannel::Nightly);
    assert!(reopened
      .install_store_release(&next, &hash, nightly.clone(), Some(&hash))
      .is_err());
    if original.as_ref() == Some(&stable) {
      let wrong_store = StoreSource {
        store_url: "https://other.example".into(),
        ..nightly.clone()
      };
      assert!(reopened
        .install_store_release(&next, &hash, wrong_store, None)
        .is_err());
      reopened
        .install_store_release(&next, &hash, store_source(StoreChannel::Stable), None)
        .unwrap();
    } else {
      assert!(reopened
        .install_store_release(&next, &hash, nightly, None)
        .is_err());
      if original.as_ref() == Some(&github) {
        assert!(reopened
          .install_with_source(&other, None, original.clone())
          .is_err());
        assert!(reopened
          .install_with_source(&next, None, Some(source(11, "sample.apel").into()))
          .is_err());
      }
      reopened
        .install_with_source(&first, None, original.clone())
        .unwrap();
    }
    assert!(reopened.list_report().unwrap().retained.is_empty());
    assert!(reopened.load("sample").is_ok());
  }
}

#[test]
fn a_store_source_switch_requires_the_reviewed_active_or_retained_snapshot() {
  let dir = tempfile::tempdir().unwrap();
  let store = BundleStore::new(dir.path().into());
  let current = package("sample", "2.0.0");
  let published = archive(&[
    ("manifest.json", &manifest("sample", "2.0.0")),
    ("index.html", b"<h1>Store version</h1>"),
  ]);
  let published_hash = digest(&published);
  let older = package("sample", "1.0.0");
  let snapshot = StoreSwitchSnapshot {
    sha256: digest(&current),
    source: None,
    removed: false,
  };
  let destination = store_source(StoreChannel::Stable);
  store.install(&current, &snapshot.sha256).unwrap();
  assert!(store
    .install_store_release(
      &current,
      &snapshot.sha256,
      destination.clone(),
      Some(&snapshot.sha256)
    )
    .is_err());
  assert!(store
    .switch_to_store_release(&older, &digest(&older), destination.clone(), &snapshot)
    .is_err());
  let switched = store
    .switch_to_store_release(&published, &published_hash, destination.clone(), &snapshot)
    .unwrap();
  assert_eq!(switched.sha256, published_hash);
  assert_eq!(switched.source, Some(destination.clone().into()));
  let other_store = StoreSource {
    store_url: "https://other.example".into(),
    ..destination.clone()
  };
  let active = StoreSwitchSnapshot {
    sha256: switched.sha256,
    source: switched.source,
    removed: false,
  };
  assert!(store
    .switch_to_store_release(&current, &snapshot.sha256, destination, &active)
    .is_err());
  assert!(store
    .switch_to_store_release(&current, &snapshot.sha256, other_store.clone(), &snapshot)
    .is_err());
  store.remove("sample", || Ok(())).unwrap();
  assert!(store
    .switch_to_store_release(&current, &snapshot.sha256, other_store.clone(), &active)
    .is_err());
  let reviewed = store
    .list_report()
    .unwrap()
    .retained
    .remove("sample")
    .unwrap();
  let reinstalled = store
    .switch_to_store_release(&current, &snapshot.sha256, other_store.clone(), &reviewed)
    .unwrap();
  assert_eq!(reinstalled.source, Some(other_store.into()));
  assert!(store.list_report().unwrap().retained.is_empty());
}

#[test]
fn removal_validates_the_identity_and_runs_cleanup_only_with_a_verified_record() {
  let dir = tempfile::tempdir().unwrap();
  let store = BundleStore::new(dir.path().into());
  let first = package("sample", "1.0.0");
  let hash = digest(&first);
  store.install(&first, &hash).unwrap();
  for id in ["", "../sample", ".install-lock", "missing"] {
    assert!(store
      .remove(id, || panic!("no verified installation"))
      .is_err());
  }
  assert!(store
    .remove("sample", || Err("cleanup failed".into()))
    .is_err());
  assert_eq!(store.installed("sample").unwrap().sha256, hash);
  let directory = dir.path().join("sample");
  let pointer = fs::read(directory.join("active")).unwrap();
  let missing_source = serde_json::json!({"applet_id": "sample", "sha256": hash}).to_string();
  for broken in ["broken", &missing_source] {
    fs::write(directory.join("active"), broken).unwrap();
    assert!(store
      .remove("sample", || panic!("unknown provenance"))
      .is_err());
  }
  assert_eq!(
    fs::read(directory.join(format!("{hash}.apel"))).unwrap(),
    first
  );

  fs::write(directory.join("active"), pointer).unwrap();
  fs::write(directory.join(format!("{hash}.apel")), b"corrupt package").unwrap();
  assert_eq!(store.list_report().unwrap().unavailable.len(), 1);
  store.remove("sample", || Ok(())).unwrap();
  assert!(store.list_report().unwrap().unavailable.is_empty());
  // An interrupted cleanup can leave old, even corrupt, archives behind.
  fs::write(directory.join(format!("{hash}.apel")), b"leftover").unwrap();
  store.install(&first, &hash).unwrap();
  assert_eq!(store.load("sample").unwrap().manifest.version, "1.0.0");
}

#[cfg(unix)]
#[test]
fn removal_never_operates_through_a_symlinked_applet_directory() {
  let dir = tempfile::tempdir().unwrap();
  let root = dir.path().join("bundles");
  let store = BundleStore::new(root.clone());
  let bytes = package("sample", "1.0.0");
  store.install(&bytes, &digest(&bytes)).unwrap();
  let directory = dir.path().join("bundles/sample");
  let outside = dir.path().join("outside");
  fs::rename(&directory, &outside).unwrap();
  std::os::unix::fs::symlink(&outside, &directory).unwrap();
  let pointer = fs::read(outside.join("active")).unwrap();
  assert!(store
    .remove("sample", || panic!("symlinked installation"))
    .is_err());
  assert_eq!(fs::read(outside.join("active")).unwrap(), pointer);
  assert_eq!(fs::read_dir(&outside).unwrap().count(), 2);
  fs::remove_file(&directory).unwrap();
  fs::rename(&outside, &directory).unwrap();

  fs::rename(directory.join("active"), &outside).unwrap();
  std::os::unix::fs::symlink(&outside, directory.join("active")).unwrap();
  assert!(store
    .remove("sample", || panic!("symlinked pointer"))
    .is_err());
  assert_eq!(fs::read(&outside).unwrap(), pointer);
  fs::remove_file(directory.join("active")).unwrap();
  fs::rename(outside, directory.join("active")).unwrap();

  let link = dir.path().join("link");
  std::os::unix::fs::symlink(root, &link).unwrap();
  assert!(BundleStore::new(link)
    .remove("sample", || panic!("symlinked root"))
    .is_err());
}

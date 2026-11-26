use super::*;
use std::fs;

#[test]
fn exact_repair_restores_missing_and_damaged_archives_without_rebinding_sources() {
  for source in [
    None,
    Some(source(10, "sample.apel").into()),
    Some(store_source(StoreChannel::Stable).into()),
  ] {
    let root = tempfile::tempdir().unwrap();
    let store = BundleStore::new(root.path().into());
    let original = package("sample", "1.0.0");
    let installed = store
      .install_with_source(&original, None, source.clone())
      .unwrap();
    let directory = root.path().join("sample");
    let pointer = directory.join("active");
    let record = fs::read(&pointer).unwrap();
    let path = directory.join(format!("{}.apel", installed.sha256));
    fs::remove_file(&path).unwrap();

    let report = store.list_report().unwrap();
    let [unavailable] = &report.unavailable[..] else {
      panic!("expected exactly one unavailable applet");
    };
    assert!(unavailable.can_remove);
    assert!(unavailable.error.contains("missing"));
    let expected = unavailable.restore_snapshot.as_ref().unwrap();
    assert_eq!(expected.sha256, installed.sha256);
    assert_eq!(expected.source, source);
    assert!(!expected.removed);

    assert!(store
      .restore_package("sample", expected, &package("sample", "2.0.0"))
      .is_err());
    assert!(store
      .restore_package("another", expected, &original)
      .is_err());
    assert!(!path.exists());
    let repaired = store
      .restore_package("sample", expected, &original)
      .unwrap();
    assert_eq!(repaired.source, source);
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::read(&pointer).unwrap(), record);
    assert!(store.list_report().unwrap().unavailable.is_empty());

    fs::write(&path, b"damaged archive").unwrap();
    let report = store.list_report().unwrap();
    assert!(report.unavailable[0].error.contains("integrity"));
    assert_eq!(
      report.unavailable[0].restore_snapshot.as_ref(),
      Some(expected)
    );
    store
      .restore_package("sample", expected, &original)
      .unwrap();
    store
      .restore_package("sample", expected, &original)
      .unwrap();
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(fs::read(&pointer).unwrap(), record);
  }
}

#[test]
fn exact_repair_rejects_stale_source_and_removed_installation() {
  let root = tempfile::tempdir().unwrap();
  let store = BundleStore::new(root.path().into());
  let original = package("sample", "1.0.0");
  let hash = digest(&original);
  store.install(&original, &hash).unwrap();
  let original_snapshot = StoreSwitchSnapshot {
    sha256: hash.clone(),
    source: None,
    removed: false,
  };
  let source = store_source(StoreChannel::Stable);
  store
    .switch_to_store_release(&original, &hash, source.clone(), &original_snapshot)
    .unwrap();
  let path = root.path().join("sample").join(format!("{hash}.apel"));
  fs::write(&path, b"damaged archive").unwrap();
  let report = store.list_report().unwrap();
  let expected = report.unavailable[0].restore_snapshot.as_ref().unwrap();
  assert_eq!(expected.source, Some(source.into()));
  assert!(store
    .restore_package("sample", &original_snapshot, &original)
    .is_err());
  assert_eq!(fs::read(&path).unwrap(), b"damaged archive");
  store.remove("sample", || Ok(())).unwrap();
  assert!(store
    .restore_package("sample", expected, &original)
    .is_err());
  assert!(!path.exists());
  assert!(store.list_report().unwrap().retained["sample"].removed);
}

#[test]
fn exact_repair_requires_a_current_api_archive() {
  let root = tempfile::tempdir().unwrap();
  let store = BundleStore::new(root.path().into());
  let directory = root.path().join("sample");
  fs::create_dir(&directory).unwrap();
  let mut unsupported: serde_json::Value =
    serde_json::from_slice(&manifest("sample", "1.0.0")).unwrap();
  unsupported["api_version"] = 100.into();
  let archive = archive(&[
    ("manifest.json", &serde_json::to_vec(&unsupported).unwrap()),
    ("index.html", b"unsupported"),
  ]);
  let hash = digest(&archive);
  let record = serde_json::json!({"applet_id":"sample","sha256":hash,"source":null});
  fs::write(directory.join("active"), record.to_string()).unwrap();
  let path = directory.join(format!("{hash}.apel"));
  fs::write(&path, &archive).unwrap();
  let report = store.list_report().unwrap();
  assert!(report.unavailable[0].can_remove);
  assert!(report.unavailable[0].error.contains("API version"));
  assert!(report.unavailable[0].restore_snapshot.is_none());
  fs::remove_file(&path).unwrap();
  let report = store.list_report().unwrap();
  let expected = report.unavailable[0].restore_snapshot.as_ref().unwrap();
  assert!(store
    .restore_package("sample", expected, &archive)
    .unwrap_err()
    .contains("API version"));
  assert!(!path.exists());

  fs::write(directory.join("active"), b"invalid pointer").unwrap();
  let invalid = store.list_report().unwrap();
  assert!(!invalid.unavailable[0].can_remove);
  assert!(invalid.unavailable[0].restore_snapshot.is_none());
  fs::write(directory.join("active"), hash).unwrap();
  let legacy = store.list_report().unwrap();
  assert!(legacy.unavailable[0].can_remove);
  assert!(legacy.unavailable[0].restore_snapshot.is_none());
}

#[cfg(unix)]
#[test]
fn exact_repair_does_not_follow_or_replace_symlinks_or_directories() {
  let root = tempfile::tempdir().unwrap();
  let store = BundleStore::new(root.path().into());
  let original = package("sample", "1.0.0");
  let hash = digest(&original);
  store.install(&original, &hash).unwrap();
  let path = root.path().join("sample").join(format!("{hash}.apel"));
  fs::remove_file(&path).unwrap();
  let report = store.list_report().unwrap();
  let expected = report.unavailable[0].restore_snapshot.as_ref().unwrap();
  let other_file = root.path().join("outside");
  fs::write(&other_file, &original).unwrap();
  std::os::unix::fs::symlink(&other_file, &path).unwrap();
  let report = store.list_report().unwrap();
  assert!(report.unavailable[0].error.contains("regular file"));
  assert!(report.unavailable[0].restore_snapshot.is_none());
  assert!(store
    .restore_package("sample", expected, &original)
    .is_err());
  assert_eq!(fs::read_link(&path).unwrap(), other_file);
  fs::remove_file(&path).unwrap();
  fs::create_dir(&path).unwrap();
  assert!(store.list_report().unwrap().unavailable[0]
    .restore_snapshot
    .is_none());
  assert!(store
    .restore_package("sample", expected, &original)
    .is_err());
  assert!(path.is_dir());
  assert_eq!(fs::read(other_file).unwrap(), original);
}

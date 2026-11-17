use super::*;
use std::fs;

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
    assert_eq!(report.retained_sources.get("sample"), Some(original));
    assert!(serde_json::to_value(report)
      .unwrap()
      .get("retained_sources")
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
    let preview = store_source(StoreChannel::Preview);
    assert!(reopened
      .install_store_release(&next, &hash, preview.clone(), Some(&hash))
      .is_err());
    if original.as_ref() == Some(&stable) {
      let wrong_store = StoreSource {
        store_url: "https://other.example".into(),
        ..preview.clone()
      };
      assert!(reopened
        .install_store_release(&next, &hash, wrong_store, None)
        .is_err());
      reopened
        .install_store_release(&next, &hash, preview, None)
        .unwrap();
    } else {
      assert!(reopened
        .install_store_release(&next, &hash, preview, None)
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
    assert!(reopened.list_report().unwrap().retained_sources.is_empty());
    assert!(reopened.load("sample").is_ok());
  }
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

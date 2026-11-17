use super::*;
use crate::icons::{png_data_url, tests::png, MAX_ICON_BYTES};

fn package_icon(version: &str, icon: Option<&[u8]>) -> Vec<u8> {
  let manifest = manifest("sample", version);
  let mut entries = vec![
    ("manifest.json", manifest.as_slice()),
    ("index.html", b"<h1>Sample</h1>".as_slice()),
  ];
  if let Some(bytes) = icon {
    entries.push(("icon.png", bytes));
  }
  archive(&entries)
}

fn metadata(store: &BundleStore) -> Vec<InstalledApplet> {
  let report = store.list_report().unwrap();
  assert!(report.unavailable.is_empty());
  let mut entries = report.applets;
  entries.extend(store.list().unwrap());
  entries.push(store.installed("sample").unwrap());
  entries.push(store.load_with_metadata("sample").unwrap().1);
  assert_eq!(entries.len(), 4);
  entries
}

#[test]
fn package_icon_is_exposed_from_every_metadata_entry_point() {
  let directory = tempfile::tempdir().unwrap();
  let store = BundleStore::new(directory.path().into());
  let icon = png(256, 256, 127);
  let bytes = package_icon("1.0.0", Some(&icon));
  let hash = digest(&bytes);
  let expected_icon = png_data_url(&icon).unwrap();
  let bound = source(10, "sample.apel");
  let installed = store
    .install_with_source(&bytes, Some(&hash), Some(bound.clone().into()))
    .unwrap();
  for entry in std::iter::once(installed).chain(metadata(&store)) {
    assert_eq!(entry.manifest.version, "1.0.0");
    assert_eq!(entry.sha256, hash);
    assert_eq!(entry.source, Some(bound.clone().into()));
    assert_eq!(entry.icon_data_url.as_deref(), Some(expected_icon.as_str()));
    assert_eq!(
      serde_json::to_value(entry).unwrap()["icon_data_url"],
      expected_icon
    );
  }
}

#[test]
fn missing_invalid_and_oversized_package_icons_fall_back_without_hiding_applets() {
  let valid = png(1, 1, 0);
  let mut oversized = valid.clone();
  oversized.resize(MAX_ICON_BYTES + 1, 0);
  for icon in [
    None,
    Some(Vec::new()),
    Some(b"<svg>not a PNG</svg>".to_vec()),
    Some(valid[..33].to_vec()),
    Some(oversized),
    Some(png(1025, 1, 0)),
    Some(png(1, 1025, 0)),
  ] {
    let directory = tempfile::tempdir().unwrap();
    let store = BundleStore::new(directory.path().into());
    let bytes = package_icon("1.0.0", icon.as_deref());
    let installed = store.install(&bytes, &digest(&bytes)).unwrap();
    for entry in std::iter::once(installed).chain(metadata(&store)) {
      assert_eq!(entry.manifest.version, "1.0.0");
      assert_eq!(entry.icon_data_url, None);
      assert!(serde_json::to_value(entry).unwrap()["icon_data_url"].is_null());
    }
    assert_eq!(store.load("sample").unwrap().html, "<h1>Sample</h1>");
  }
}

#[test]
fn updates_replace_or_remove_icons_without_mixing_loaded_package_snapshots() {
  let directory = tempfile::tempdir().unwrap();
  let store = BundleStore::new(directory.path().into());
  let first_icon = png(1, 1, 0);
  let next_icon = png(1, 1, 255);
  let first = package_icon("1.0.0", Some(&first_icon));
  let next = package_icon("2.0.0", Some(&next_icon));
  let bound = source(10, "sample.apel");
  store
    .install_with_source(&first, None, Some(bound.clone().into()))
    .unwrap();
  let (loaded, snapshot) = store.load_with_metadata("sample").unwrap();
  let updated = store
    .update_from_source("sample", &next, None, bound.clone().into())
    .unwrap();
  for entry in std::iter::once(updated).chain(metadata(&store)) {
    assert_eq!(entry.manifest.version, "2.0.0");
    assert_eq!(entry.sha256, digest(&next));
    assert_eq!(entry.icon_data_url, Some(png_data_url(&next_icon).unwrap()));
  }
  assert_eq!(loaded.manifest.version, "1.0.0");
  assert_eq!(loaded.read_asset("icon.png"), Some(first_icon.as_slice()));
  assert_eq!(snapshot.sha256, digest(&first));
  assert_eq!(
    snapshot.icon_data_url,
    Some(png_data_url(&first_icon).unwrap())
  );

  let without_icon = package_icon("3.0.0", None);
  let updated = store
    .update_from_source("sample", &without_icon, None, bound.into())
    .unwrap();
  for entry in std::iter::once(updated).chain(metadata(&store)) {
    assert_eq!(entry.manifest.version, "3.0.0");
    assert_eq!(entry.sha256, digest(&without_icon));
    assert_eq!(entry.icon_data_url, None);
  }
}

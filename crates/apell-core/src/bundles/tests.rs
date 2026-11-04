use super::*;
use std::{
  io::{Cursor, Write},
  sync::{Arc, Barrier},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

fn manifest(id: &str, version: &str) -> Vec<u8> {
  serde_json::to_vec(&serde_json::json!({"schema_version":1,"api_version":2,"applet_id":id,"name":"Sample","version":version,"permissions":["sqlite"]})).unwrap()
}
fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
  let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
  let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
  for (path, bytes) in entries {
    zip.start_file(*path, options).unwrap();
    zip.write_all(bytes).unwrap();
  }
  zip.finish().unwrap().into_inner()
}
fn package(id: &str, version: &str) -> Vec<u8> {
  archive(&[
    ("manifest.json", &manifest(id, version)),
    ("index.html", b"<h1>Sample</h1>"),
    ("assets/ui.js", b"export default 1"),
  ])
}
fn source(id: u64, asset: &str) -> GitHubSource {
  GitHubSource {
    repository_id: id,
    repository: "sample/project".into(),
    asset_name: asset.into(),
    release_tag: None,
    prerelease: false,
  }
}
fn tagged_source(tag: &str, prerelease: bool) -> GitHubSource {
  GitHubSource {
    release_tag: Some(tag.into()),
    prerelease,
    ..source(10, "sample.apel")
  }
}
fn replace_all(bytes: &mut [u8], old: &[u8], new: &[u8]) {
  assert_eq!(old.len(), new.len());
  let positions: Vec<_> = bytes
    .windows(old.len())
    .enumerate()
    .filter_map(|(i, s)| (s == old).then_some(i))
    .collect();
  assert!(!positions.is_empty());
  for i in positions {
    bytes[i..i + old.len()].copy_from_slice(new);
  }
}

#[test]
fn deterministic_packer_preserves_html_assets_and_declared_guest_module() {
  let dir = tempfile::tempdir().unwrap();
  let root = dir.path();
  std::fs::create_dir(root.join("assets")).unwrap();
  std::fs::create_dir(root.join("lib")).unwrap();
  let mut m: serde_json::Value = serde_json::from_slice(&manifest("sample", "1.2.3")).unwrap();
  m["lib"] = "lib/main.wasm".into();
  std::fs::write(root.join("manifest.json"), m.to_string()).unwrap();
  std::fs::write(
    root.join("index.html"),
    "<script src=\"assets/main.js\"></script>",
  )
  .unwrap();
  std::fs::write(root.join("assets/main.js"), b"test").unwrap();
  std::fs::write(root.join("lib/main.wasm"), b"\0asm\x01\0\0\0").unwrap();
  let first = pack_directory(root).unwrap();
  assert_eq!(first, pack_directory(root).unwrap());
  let bundle = parse(&first).unwrap();
  assert_eq!(bundle.manifest.lib.as_deref(), Some("lib/main.wasm"));
  assert_eq!(
    bundle.read_asset("assets/main.js"),
    Some(b"test".as_slice())
  );
  assert_eq!(bundle.html, "<script src=\"assets/main.js\"></script>");
  std::fs::remove_file(root.join("lib/main.wasm")).unwrap();
  assert!(pack_directory(root).is_err());
}

#[test]
fn rejects_unknown_versions_permissions_and_unsafe_or_missing_backend() {
  for (field, value) in [
    ("api_version", serde_json::json!(1)),
    ("version", serde_json::json!("one")),
    ("applet_id", serde_json::json!("../escape")),
    ("permissions", serde_json::json!(["shell"])),
    ("permissions", serde_json::json!(["llm", "llm"])),
    ("lib", serde_json::json!("assets/guest.wasm")),
    ("lib", serde_json::json!("lib/missing.wasm")),
    ("lib", serde_json::json!("lib/../index.html")),
  ] {
    let mut m: serde_json::Value = serde_json::from_slice(&manifest("sample", "1.0.0")).unwrap();
    m[field] = value;
    assert!(parse(&archive(&[
      ("manifest.json", &serde_json::to_vec(&m).unwrap()),
      ("index.html", b"html")
    ]))
    .is_err());
  }
  assert!(parse(&archive(&[
    ("manifest.json", &manifest("sample", "1.0.0")),
    ("index.html", b"\xff")
  ]))
  .is_err());
}

#[test]
fn denies_traversal_invalid_utf8_duplicate_case_collisions_and_conflicting_file_parents() {
  let m = manifest("sample", "1.0.0");
  for invalid in [
    "../outside",
    "/absolute",
    "C:/private",
    "assets\\outside",
    "assets//file",
    "assets/./file",
    "assets/%2e%2e/escape",
  ] {
    assert!(parse(&archive(&[
      ("manifest.json", &m),
      ("index.html", b"html"),
      (invalid, b"test")
    ]))
    .is_err());
  }
  for (a, b) in [
    ("assets/A.js", "assets/a.js"),
    ("assets/Foo/file", "assets/foo/image"),
    ("assets/Straße/file", "assets/STRASSE/image"),
    ("assets/é/file", "assets/e\u{301}/image"),
    ("assets/foo", "assets/foo/image"),
  ] {
    assert!(parse(&archive(&[
      ("manifest.json", &m),
      ("index.html", b"html"),
      (a, b"1"),
      (b, b"2")
    ]))
    .is_err());
  }
  let mut duplicate = archive(&[
    ("manifest.json", &m),
    ("index.html", b"html"),
    ("assets/one", b"A"),
    ("assets/two", b"B"),
  ]);
  replace_all(&mut duplicate, b"assets/two", b"assets/one");
  assert!(parse(&duplicate).is_err());
  let mut invalid = package("sample", "1.0.0");
  replace_all(&mut invalid, b"assets/ui.js", b"assets/\xffi.js");
  assert!(parse(&invalid).is_err());
}

#[test]
fn rejects_symlinks_encryption_and_local_header_disagreement() {
  let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
  let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
  zip.start_file("manifest.json", options).unwrap();
  zip.write_all(&manifest("sample", "1.0.0")).unwrap();
  zip.start_file("index.html", options).unwrap();
  zip.write_all(b"html").unwrap();
  zip
    .add_symlink("assets/link", "../index.html", options)
    .unwrap();
  assert!(parse(&zip.finish().unwrap().into_inner()).is_err());
  let mut bytes = package("sample", "1.0.0");
  let central = bytes.windows(4).position(|s| s == b"PK\x01\x02").unwrap();
  bytes[6] |= 1;
  bytes[central + 8] |= 1;
  assert!(parse(&bytes).is_err());
  let mut different = package("sample", "1.0.0");
  different[30] = b'/';
  assert!(parse(&different).is_err());
}

#[test]
fn rejects_oversized_archives_and_excess_directory_records_before_decoding() {
  let bytes = vec![0u8; MAX_BUNDLE_BYTES + 1];
  assert!(parse(&bytes).unwrap_err().contains("24 MiB"));
  let mut bytes = package("sample", "1.0.0");
  let end = bytes.len() - 22;
  assert_eq!(&bytes[end..end + 4], b"PK\x05\x06");
  let count = (MAX_ENTRIES as u16 + 1).to_le_bytes();
  bytes[end + 8..end + 10].copy_from_slice(&count);
  bytes[end + 10..end + 12].copy_from_slice(&count);
  assert!(parse(&bytes).is_err());
}

#[cfg(unix)]
#[test]
fn packer_refuses_links_to_files_and_directories_outside_the_input() {
  let dir = tempfile::tempdir().unwrap();
  let package = dir.path().join("package");
  let external = dir.path().join("external");
  std::fs::create_dir(&package).unwrap();
  std::fs::create_dir(&external).unwrap();
  std::fs::write(package.join("manifest.json"), manifest("sample", "1.0.0")).unwrap();
  std::fs::write(package.join("index.html"), "<p>hello</p>").unwrap();
  std::fs::write(external.join("asset"), "fixture").unwrap();
  for target in [external.join("asset"), external] {
    std::os::unix::fs::symlink(target, package.join("link")).unwrap();
    assert!(pack_directory(&package).unwrap_err().contains("symlinks"));
    std::fs::remove_file(package.join("link")).unwrap();
  }
}

#[test]
fn actual_expansion_is_bounded_even_when_zip_metadata_understates_the_payload() {
  let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
  let options = SimpleFileOptions::default()
    .compression_method(CompressionMethod::Deflated)
    .compression_level(Some(1));
  zip.start_file("manifest.json", options).unwrap();
  zip.write_all(&manifest("sample", "1.0.0")).unwrap();
  zip.start_file("index.html", options).unwrap();
  zip.write_all(b"html").unwrap();
  zip.start_file("assets/large", options).unwrap();
  let chunk = [0; 64 * 1024];
  for _ in 0..MAX_EXPANDED_BYTES / chunk.len() {
    zip.write_all(&chunk).unwrap();
  }
  let mut bytes = zip.finish().unwrap().into_inner();
  let name = b"assets/large";
  let headers: Vec<_> = bytes
    .windows(name.len())
    .enumerate()
    .filter_map(|(i, s)| (s == name).then_some(i))
    .collect();
  assert_eq!(headers.len(), 2);
  bytes[headers[0] - 30 + 22..headers[0] - 30 + 26].copy_from_slice(&1u32.to_le_bytes());
  bytes[headers[1] - 46 + 24..headers[1] - 46 + 28].copy_from_slice(&1u32.to_le_bytes());
  assert!(bytes.len() < MAX_BUNDLE_BYTES);
  assert_eq!(
    parse(&bytes).unwrap_err(),
    "Package exceeds the 64 MiB expanded size limit."
  );
}

#[test]
fn verifies_hash_and_reads_existing_legacy_cache_but_never_installs_legacy_json() {
  let dir = tempfile::tempdir().unwrap();
  let store = BundleStore::new(dir.path().into());
  let old = serde_json::json!({"manifest":{"schema_version":1,"api_version":1,"applet_id":"sample","name":"Sample","version":"1","permissions":[]},"html":"legacy"}).to_string();
  let hash = digest(old.as_bytes());
  assert!(store.install(old.as_bytes(), &hash).is_err());
  let app = dir.path().join("sample");
  std::fs::create_dir(&app).unwrap();
  std::fs::write(app.join(format!("{hash}.json")), old).unwrap();
  std::fs::write(app.join("active"), hash).unwrap();
  let cached = store.load("sample").unwrap();
  assert_eq!(cached.html, "legacy");
  assert_eq!(cached.read_asset("index.html"), Some(b"legacy".as_slice()));
  let bytes = package("sample", "2.0.0");
  assert!(store.install(&bytes, &"0".repeat(64)).is_err());
  assert_eq!(store.load("sample").unwrap().html, "legacy");
  let next = store.install_with_source(&bytes, None, None).unwrap();
  assert!(next.source.is_none());
  assert_eq!(store.load("sample").unwrap().manifest.api_version, 2);
  std::fs::write(app.join(format!("{}.apel", next.sha256)), b"changed").unwrap();
  assert!(store.load("sample").is_err());
  assert!(store.install(&bytes, &next.sha256).is_err());
}

#[test]
fn binds_release_repo_asset_and_applet_id_and_rejects_replacements_or_downgrades() {
  let dir = tempfile::tempdir().unwrap();
  let store = BundleStore::new(dir.path().into());
  let bound = source(10, "sample.apel");
  let first = package("sample", "1.0.0");
  store
    .install_with_source(&first, None, Some(bound.clone()))
    .unwrap();
  for foreign in [source(11, "sample.apel"), source(10, "another.apel")] {
    assert!(store
      .install_with_source(&package("sample", "2.0.0"), None, Some(foreign))
      .is_err());
  }
  assert!(store
    .install_with_source(&package("elsewhere", "2.0.0"), None, Some(bound.clone()))
    .is_err());
  let changed = archive(&[
    ("manifest.json", &manifest("sample", "1.0.0")),
    ("index.html", b"replacement"),
  ]);
  assert!(store
    .install_with_source(&changed, None, Some(bound.clone()))
    .is_err());
  let next = package("sample", "2.0.0");
  let updated = store
    .update_from_source("sample", &next, None, bound.clone())
    .unwrap();
  assert!(store
    .install_with_source(&first, None, Some(bound.clone()))
    .is_err());
  assert_eq!(store.list().unwrap()[0].sha256, updated.sha256);
  assert_eq!(
    store.installed("sample").unwrap().source,
    Some(bound.clone())
  );
  assert_eq!(
    store.install_with_source(&next, None, None).unwrap().source,
    Some(bound.clone())
  );
  assert!(store
    .install_with_source(&package("sample", "3.0.0"), None, None)
    .unwrap()
    .source
    .is_none());
  // Matching public bytes are not permission to bind future updates to a repo.
  assert!(store
    .install_with_source(&package("sample", "3.0.0"), None, Some(bound.clone()))
    .is_err());
  assert!(store
    .update_from_source("sample", &package("sample", "4.0.0"), None, bound.clone())
    .is_err());
  assert!(store
    .update_from_source(
      "sample",
      &package("elsewhere", "4.0.0"),
      None,
      bound.clone()
    )
    .is_err());
  assert!(store
    .install_with_source(&package("sample", "4.0.0"), None, Some(bound))
    .is_err());
}

#[test]
fn rolling_prerelease_updates_equal_versions_and_retains_its_channel_on_reopen() {
  let dir = tempfile::tempdir().unwrap();
  let first = package("sample", "1.0.0");
  let bound = tagged_source("nightly", true);
  BundleStore::new(dir.path().into())
    .install_with_source(&first, None, Some(bound.clone()))
    .unwrap();
  let store = BundleStore::new(dir.path().into());
  assert_eq!(
    store.installed("sample").unwrap().source,
    Some(bound.clone())
  );
  let changed = archive(&[
    ("manifest.json", &manifest("sample", "1.0.0")),
    ("index.html", b"nightly replacement"),
  ]);
  let updated = store
    .update_from_source("sample", &changed, None, bound.clone())
    .unwrap();
  assert_ne!(updated.sha256, digest(&first));
  assert_eq!(updated.source, Some(bound.clone()));
  assert_eq!(store.load("sample").unwrap().html, "nightly replacement");
  assert!(store
    .update_from_source("sample", &first, None, tagged_source("nightly", false))
    .is_err());
  assert!(store
    .update_from_source("sample", &package("sample", "0.9.0"), None, bound.clone())
    .is_err());
  assert!(store
    .update_from_source("sample", &package("another", "2.0.0"), None, bound.clone())
    .is_err());
  for foreign in [
    GitHubSource {
      repository_id: 11,
      ..bound.clone()
    },
    GitHubSource {
      asset_name: "another.apel".into(),
      ..bound.clone()
    },
  ] {
    assert!(store
      .update_from_source("sample", &package("sample", "2.0.0"), None, foreign)
      .is_err());
  }
  let reopened = BundleStore::new(dir.path().into());
  assert_eq!(reopened.installed("sample").unwrap().source, Some(bound));
  assert_eq!(reopened.installed("sample").unwrap().sha256, updated.sha256);
}

#[test]
fn stable_release_cannot_gain_same_version_replacement_through_prerelease_metadata() {
  let dir = tempfile::tempdir().unwrap();
  let store = BundleStore::new(dir.path().into());
  let first = package("sample", "1.0.0");
  let stable = tagged_source("preview", false);
  let prerelease = tagged_source("preview", true);
  store
    .install_with_source(&first, None, Some(stable.clone()))
    .unwrap();
  let changed = archive(&[
    ("manifest.json", &manifest("sample", "1.0.0")),
    ("index.html", b"changed release"),
  ]);
  for bytes in [&first, &changed] {
    assert!(store
      .update_from_source("sample", bytes, None, prerelease.clone())
      .is_err());
    assert_eq!(
      store.installed("sample").unwrap().source,
      Some(stable.clone())
    );
    assert_eq!(store.installed("sample").unwrap().sha256, digest(&first));
  }
  assert!(store
    .update_from_source("sample", &changed, None, stable)
    .is_err());
  store
    .update_from_source(
      "sample",
      &package("sample", "2.0.0"),
      None,
      prerelease.clone(),
    )
    .unwrap();
  assert_eq!(store.installed("sample").unwrap().source, Some(prerelease));
}

#[test]
fn release_tags_are_bound_to_the_installation_and_prereleases_require_an_explicit_tag() {
  let dir = tempfile::tempdir().unwrap();
  let store = BundleStore::new(dir.path().into());
  let first = package("sample", "1.0.0");
  for invalid in [
    GitHubSource {
      prerelease: true,
      ..source(10, "sample.apel")
    },
    tagged_source("", true),
    tagged_source("nightly\n", true),
  ] {
    assert!(store
      .install_with_source(&first, None, Some(invalid))
      .is_err());
  }
  let bound = tagged_source("nightly", true);
  store
    .install_with_source(&first, None, Some(bound.clone()))
    .unwrap();
  for foreign in [tagged_source("preview", true), source(10, "sample.apel")] {
    let next = package("sample", "2.0.0");
    assert!(store
      .install_with_source(&next, None, Some(foreign.clone()))
      .is_err());
    assert!(store
      .update_from_source("sample", &next, None, foreign)
      .is_err());
  }
  assert_eq!(store.installed("sample").unwrap().source, Some(bound));
  assert_eq!(store.installed("sample").unwrap().sha256, digest(&first));
}

#[test]
fn persisted_sources_without_channel_fields_remain_latest_stable() {
  let dir = tempfile::tempdir().unwrap();
  let store = BundleStore::new(dir.path().into());
  let bound = source(10, "sample.apel");
  let installed = store
    .install_with_source(&package("sample", "1.0.0"), None, Some(bound.clone()))
    .unwrap();
  // Reproduce the persisted record written before release channels existed.
  let old = serde_json::json!({
    "applet_id": "sample",
    "sha256": installed.sha256,
    "source": {"repository_id": 10, "repository": "sample/project", "asset_name": "sample.apel"}
  });
  std::fs::write(
    dir.path().join("sample/active"),
    serde_json::to_vec(&old).unwrap(),
  )
  .unwrap();
  let reopened = BundleStore::new(dir.path().into());
  assert_eq!(
    reopened.installed("sample").unwrap().source,
    Some(bound.clone())
  );
  assert!(reopened
    .update_from_source(
      "sample",
      &package("sample", "2.0.0"),
      None,
      tagged_source("nightly", true)
    )
    .is_err());
  let updated = reopened
    .update_from_source("sample", &package("sample", "2.0.0"), None, bound.clone())
    .unwrap();
  assert_eq!(updated.source, Some(bound));
}

#[test]
fn concurrent_sources_cannot_claim_the_same_applet_id() {
  let dir = tempfile::tempdir().unwrap();
  let bytes = package("sample", "1.0.0");
  let barrier = Arc::new(Barrier::new(2));
  let winners = std::thread::scope(|scope| {
    let jobs: Vec<_> = [10, 11]
      .into_iter()
      .map(|id| {
        let gate = barrier.clone();
        let bytes = &bytes;
        let store = BundleStore::new(dir.path().into());
        scope.spawn(move || {
          gate.wait();
          store
            .install_with_source(bytes, None, Some(source(id, "sample.apel")))
            .is_ok()
        })
      })
      .collect();
    jobs
      .into_iter()
      .map(|job| job.join().unwrap() as usize)
      .sum::<usize>()
  });
  assert_eq!(winners, 1);
}

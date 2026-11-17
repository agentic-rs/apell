use super::*;
use apell_core::bundles::{GitHubSource, Manifest};

fn release(version: &str) -> StoreRelease {
  StoreRelease {
    version: version.into(),
    sha256: "a".repeat(64),
    size_bytes: 100,
    published_at: "2026-01-01T00:00:00Z".into(),
    release_notes: String::new(),
    api_version: 2,
    permissions: vec!["sqlite".into()],
    network: true,
    has_backend: false,
    download_url: format!("/api/v1/applets/sample/versions/{version}/download"),
    prerelease: version.contains('-'),
  }
}
fn installed(version: &str) -> InstalledApplet {
  InstalledApplet {
    manifest: Manifest {
      schema_version: 1,
      api_version: 2,
      applet_id: "sample".into(),
      name: "Sample".into(),
      version: version.into(),
      permissions: vec!["sqlite".into()],
      network: true,
      lib: None,
    },
    sha256: "a".repeat(64),
    icon_data_url: None,
    source: Some(PackageSource::Store(StoreSource {
      store_url: "https://store.example".into(),
      applet_id: "sample".into(),
      channel: StoreChannel::Stable,
    })),
  }
}
fn action(release: &StoreRelease, saved: Option<&InstalledApplet>) -> ReleaseAction {
  release_action("https://store.example", "sample", release, saved, None).0
}

#[test]
fn reinstall_is_offered_only_by_the_store_that_owns_the_saved_data() {
  let mut report = AppletListReport {
    applets: vec![],
    unavailable: vec![],
    retained_sources: [("sample".into(), installed("1.0.0").source)].into(),
  };
  assert!(blocked_install_reason(&report, "https://store.example", "sample").is_none());
  assert!(blocked_install_reason(&report, "https://other.example", "sample").is_some());
  report.retained_sources.insert("sample".into(), None);
  assert!(blocked_install_reason(&report, "https://store.example", "sample").is_some());
}

#[test]
fn release_choices_preserve_source_ownership_and_never_offer_downgrades() {
  let selected = release("1.0.0");
  assert_eq!(action(&selected, None), ReleaseAction::Install);
  assert_eq!(
    action(&selected, Some(&installed("1.0.0"))),
    ReleaseAction::Installed
  );
  assert_eq!(
    action(&selected, Some(&installed("0.9.0"))),
    ReleaseAction::Update
  );
  assert_eq!(
    action(&selected, Some(&installed("2.0.0"))),
    ReleaseAction::Older
  );
  assert_eq!(
    release_action(
      "https://store.example",
      "sample",
      &selected,
      None,
      Some("Unavailable")
    )
    .0,
    ReleaseAction::Conflict
  );
  let mut saved = installed("1.0.0");
  saved.sha256 = "b".repeat(64);
  assert_eq!(action(&selected, Some(&saved)), ReleaseAction::Conflict);
  saved = installed("1.0.0+different");
  assert_eq!(action(&selected, Some(&saved)), ReleaseAction::Conflict);
  for source in [
    None,
    Some(PackageSource::Github(GitHubSource {
      repository_id: 1,
      repository: "sample/project".into(),
      asset_name: "sample.apel".into(),
      release_tag: None,
      prerelease: false,
    })),
    Some(PackageSource::Store(StoreSource {
      store_url: "https://other.example".into(),
      applet_id: "sample".into(),
      channel: StoreChannel::Stable,
    })),
  ] {
    saved = installed("0.9.0");
    saved.source = source;
    assert_eq!(action(&selected, Some(&saved)), ReleaseAction::Conflict);
  }
}

#[test]
fn update_badges_follow_the_installed_release_channel() {
  let mut applet = StoreApplet {
    applet_id: "sample".into(),
    name: "Sample".into(),
    description: "Sample applet".into(),
    project_url: None,
    icon_url: None,
    latest_release: release("2.0.0-beta.1"),
    latest_stable_release: Some(release("1.0.0")),
  };
  let mut saved = installed("1.0.0");
  assert!(!has_update(&applet, &saved));
  if let Some(PackageSource::Store(source)) = &mut saved.source {
    source.channel = StoreChannel::Preview;
  }
  assert!(has_update(&applet, &saved));
  saved = installed("0.9.0");
  assert!(has_update(&applet, &saved));
  applet.latest_stable_release = None;
  assert!(!has_update(&applet, &saved));
}

#[test]
fn local_store_settings_are_atomic_optional_and_preserve_corrupt_files() {
  let root = tempfile::tempdir().unwrap();
  assert!(state::load(root.path()).unwrap().store_url.is_none());
  state::save(
    root.path(),
    &StoreConfig {
      store_url: Some("https://store.example/".into()),
    },
  )
  .unwrap();
  assert_eq!(
    state::load(root.path()).unwrap().store_url.as_deref(),
    Some("https://store.example")
  );
  let path = root.path().join("store-settings.json");
  std::fs::write(&path, b"corrupted").unwrap();
  assert!(state::load(root.path()).is_err());
  assert_eq!(std::fs::read(&path).unwrap(), b"corrupted");
  std::fs::write(&path, vec![b' '; 4097]).unwrap();
  assert!(state::load(root.path()).is_err());
  state::save(root.path(), &StoreConfig::default()).unwrap();
  assert!(state::load(root.path()).unwrap().store_url.is_none());
  assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn store_url_changes_wait_for_the_install_gate_and_reject_old_requests() {
  let root = tempfile::tempdir().unwrap();
  let host = Host::new(root.path().into()).unwrap();
  assert!(configured(&host, "https://store.example").await.is_err());
  save_config(&host, " https://store.example/ ")
    .await
    .unwrap();
  assert_eq!(
    configured(&host, "https://store.example").await.unwrap(),
    "https://store.example"
  );
  let install_guard = host.store_gate.lock().await;
  let change = save_config(&host, "https://other.example");
  tokio::pin!(change);
  assert!(
    tokio::time::timeout(std::time::Duration::from_millis(20), change.as_mut())
      .await
      .is_err()
  );
  assert_eq!(
    state::load(root.path()).unwrap().store_url.as_deref(),
    Some("https://store.example")
  );
  drop(install_guard);
  change.await.unwrap();
  assert!(configured(&host, "https://store.example").await.is_err());
  assert_eq!(
    configured(&host, "https://other.example").await.unwrap(),
    "https://other.example"
  );
  save_config(&host, " ").await.unwrap();
  assert!(config(&host).await.unwrap().store_url.is_none());
}

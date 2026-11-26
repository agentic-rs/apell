use super::*;
use apell_core::bundles::{GitHubSource, Manifest, UnavailableApplet};

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
  let mut report = empty_report();
  report.applets.extend(saved.cloned());
  release_action(
    release,
    &review_install(&report, "https://store.example", "sample"),
  )
  .0
}

fn empty_report() -> AppletListReport {
  AppletListReport {
    applets: vec![],
    unavailable: vec![],
    retained: Default::default(),
  }
}

fn store_installation(
  id: &str,
  origin: &str,
  version: &str,
  channel: StoreChannel,
) -> InstalledApplet {
  let mut applet = installed(version);
  applet.manifest.applet_id = id.into();
  applet.source = Some(
    StoreSource {
      store_url: origin.into(),
      applet_id: id.into(),
      channel,
    }
    .into(),
  );
  applet
}

fn listing(id: &str, stable: &str, nightly: Option<&str>) -> StoreApplet {
  let release_for = |version: &str| StoreRelease {
    download_url: format!("/api/v1/applets/{id}/versions/{version}/download"),
    ..release(version)
  };
  let stable = release_for(stable);
  let nightly = nightly.map(release_for);
  let latest = nightly
    .as_ref()
    .filter(|head| head.compare_version(&stable.version).unwrap().is_gt())
    .unwrap_or(&stable);
  let applet = StoreApplet {
    applet_id: id.into(),
    name: "Catalog name".into(),
    description: "App description".into(),
    project_url: None,
    icon_url: Some(format!("/media/{}.png", "b".repeat(64))),
    latest_release: latest.clone(),
    latest_stable_release: Some(stable),
    latest_nightly_release: nightly,
  };
  applet.validate().unwrap();
  applet
}

#[tokio::test]
async fn updates_fetch_only_distinct_canonical_installed_origins() {
  let alpha = "https://a.example";
  let bravo = "https://b.example";
  let mut report = empty_report();
  let stable = |id, origin| store_installation(id, origin, "1.0.0", StoreChannel::Stable);
  let mut local = stable("file", "https://file.example");
  local.source = None;
  let mut github = stable("github", "https://github.example");
  github.source = Some(
    GitHubSource {
      repository_id: 1,
      repository: "sample/project".into(),
      asset_name: "sample.apel".into(),
      release_tag: None,
      prerelease: false,
    }
    .into(),
  );
  let mut mismatched = stable("different-id", "https://wrong.example");
  mismatched.manifest.applet_id = "mismatched".into();
  report.retained.insert(
    "removed".into(),
    StoreSwitchSnapshot {
      sha256: "a".repeat(64),
      source: stable("removed", "https://removed.example").source,
      removed: true,
    },
  );
  report.unavailable.push(UnavailableApplet {
    applet_id: "broken".into(),
    error: "Broken package".into(),
  });
  report.applets = vec![
    stable("bravo", bravo),
    stable("alpha", alpha),
    stable("neighbor", alpha),
    stable("noncanonical", "https://a.example/"),
    local,
    github,
    mismatched,
  ];
  let mut calls = std::sync::Mutex::new(Vec::new());
  let result = check_updates(report, |origin| {
    let applets = match origin.as_str() {
      "https://a.example" => vec![
        listing("alpha", "2.0.0", None),
        listing("neighbor", "1.0.0", None),
        listing("bravo", "3.0.0", None),
        listing("file", "2.0.0", None),
      ],
      "https://b.example" => vec![listing("bravo", "1.0.0", None)],
      _ => panic!("requested an origin without a verified Store installation"),
    };
    calls.lock().unwrap().push(origin);
    std::future::ready(Ok(StoreCatalog { applets }))
  })
  .await;

  calls.get_mut().unwrap().sort();
  assert_eq!(calls.into_inner().unwrap(), [alpha, bravo]);
  let [first, second] = result.sources.as_slice() else {
    panic!("expected two source groups");
  };
  assert_eq!(
    (
      &*first.store_url,
      first.installed_count,
      first.checked_count
    ),
    (alpha, 2, 2)
  );
  assert_eq!(first.updates.len(), 1);
  assert_eq!(first.updates[0].applet.manifest.applet_id, "alpha");
  assert_eq!(
    (
      &*second.store_url,
      second.installed_count,
      second.checked_count
    ),
    (bravo, 1, 1)
  );
  assert!(second.updates.is_empty());
}

#[tokio::test]
async fn updates_keep_successful_results_when_an_origin_fails() {
  let healthy = "https://healthy.example";
  let unavailable = "https://unavailable.example";
  let mut report = empty_report();
  report.applets = vec![
    store_installation("sample", healthy, "1.0.0", StoreChannel::Stable),
    store_installation("other", unavailable, "1.0.0", StoreChannel::Stable),
  ];
  let result = check_updates(report, |origin| async move {
    match origin.as_str() {
      "https://healthy.example" => Ok(StoreCatalog {
        applets: vec![listing("sample", "2.0.0", None)],
      }),
      "https://unavailable.example" => Err("Could not reach the store.".into()),
      _ => unreachable!(),
    }
  })
  .await;
  let json = output(result).unwrap();
  let sources = json["sources"].as_array().unwrap();
  assert_eq!(sources.len(), 2);
  assert_eq!(sources[0]["checked_count"], 1);
  assert_eq!(sources[0]["updates"][0]["release"]["version"], "2.0.0");
  assert!(sources[0].get("error").is_none());
  assert_eq!(
    sources[1],
    json!({
      "store_url": unavailable,
      "installed_count": 1,
      "checked_count": 0,
      "updates": [],
      "error": "Could not reach the store."
    })
  );
}

#[tokio::test]
async fn updates_follow_each_installed_channel_and_keep_local_artwork() {
  let origin = "https://store.example";
  let mut report = empty_report();
  let mut nightly = store_installation("nightly", origin, "1.0.0-beta.1", StoreChannel::Nightly);
  nightly.icon_data_url = Some("data:image/png;base64,aW5zdGFsbGVk".into());
  report.applets = vec![
    nightly,
    store_installation("stable", origin, "1.0.0", StoreChannel::Stable),
    store_installation(
      "missing-channel",
      origin,
      "1.0.0-beta.1",
      StoreChannel::Nightly,
    ),
    store_installation("unlisted", origin, "1.0.0", StoreChannel::Stable),
  ];
  let result = check_updates(report, |_| async {
    Ok(StoreCatalog {
      applets: vec![
        listing("nightly", "3.0.0", Some("2.0.0-beta.1")),
        listing("stable", "1.0.0", Some("2.0.0-beta.1")),
        listing("missing-channel", "3.0.0", None),
      ],
    })
  })
  .await;
  let source = &result.sources[0];
  assert_eq!(source.installed_count, 4);
  assert_eq!(source.checked_count, 2);
  assert_eq!(source.updates.len(), 1);
  let update = &source.updates[0];
  assert_eq!(update.applet.manifest.name, "Sample");
  assert_eq!(
    update.applet.icon_data_url.as_deref(),
    Some("data:image/png;base64,aW5zdGFsbGVk")
  );
  assert_eq!(update.release.version, "2.0.0-beta.1");
}

#[test]
fn retained_data_requires_confirmation_when_reinstalling_from_another_source() {
  let mut report = empty_report();
  let selected = release("1.0.0");
  let snapshot = StoreSwitchSnapshot {
    sha256: "a".repeat(64),
    source: installed("1.0.0").source,
    removed: true,
  };
  report.retained.insert("sample".into(), snapshot.clone());

  let original = review_install(&report, "https://store.example", "sample");
  assert_eq!(
    release_action(&selected, &original).0,
    ReleaseAction::Install
  );
  assert_eq!(
    checked_switch(&selected, &original, None, None, None).unwrap(),
    ReleaseAction::Install
  );

  let different = review_install(&report, "https://other.example", "sample");
  assert_eq!(
    release_action(&selected, &different).0,
    ReleaseAction::SwitchSource
  );
  assert!(checked_switch(&selected, &different, None, None, None).is_err());
  assert_eq!(
    checked_switch(&selected, &different, None, Some(&snapshot), None).unwrap(),
    ReleaseAction::SwitchSource
  );

  report.retained.get_mut("sample").unwrap().source = None;
  let stale = review_install(&report, "https://other.example", "sample");
  assert!(checked_switch(&selected, &stale, None, Some(&snapshot), None).is_err());
}

#[test]
fn release_choices_keep_ordinary_updates_immutable_and_never_offer_downgrades() {
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
    saved = installed("1.0.0");
    saved.sha256 = "b".repeat(64);
    saved.source = source;
    assert_eq!(action(&selected, Some(&saved)), ReleaseAction::SwitchSource);
    saved.manifest.version = "2.0.0".into();
    assert_eq!(action(&selected, Some(&saved)), ReleaseAction::Older);
    saved.manifest.version = "legacy".into();
    assert_eq!(action(&selected, Some(&saved)), ReleaseAction::Conflict);
  }
}

#[test]
fn source_switch_requires_the_reviewed_active_package_and_cannot_replace_a_broken_one() {
  let mut report = empty_report();
  let mut saved = installed("1.0.0");
  saved.source = None;
  report.applets.push(saved.clone());
  let selected = release("1.0.0");
  let review = review_install(&report, "https://store.example", "sample");
  let snapshot = review.switch_snapshot.clone().unwrap();
  assert!(checked_switch(&selected, &review, None, Some(&snapshot), None).is_err());
  assert!(checked_switch(&selected, &review, Some(&saved.sha256), None, None).is_err());
  assert_eq!(
    checked_switch(
      &selected,
      &review,
      Some(&saved.sha256),
      Some(&snapshot),
      None
    )
    .unwrap(),
    ReleaseAction::SwitchSource
  );
  report.applets[0].source = installed("1.0.0").source;
  let stale = review_install(&report, "https://store.example", "sample");
  assert!(checked_switch(
    &selected,
    &stale,
    Some(&saved.sha256),
    Some(&snapshot),
    None
  )
  .is_err());

  report.applets.clear();
  report.unavailable.push(UnavailableApplet {
    applet_id: "sample".into(),
    error: "Broken package".into(),
  });
  let broken = review_install(&report, "https://store.example", "sample");
  assert_eq!(
    release_action(&selected, &broken).0,
    ReleaseAction::Conflict
  );
  assert!(checked_switch(&selected, &broken, None, Some(&snapshot), None).is_err());
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
    latest_nightly_release: Some(release("2.0.0-beta.1")),
  };
  let mut saved = installed("1.0.0");
  assert!(!has_update(&applet, &saved));
  if let Some(PackageSource::Store(source)) = &mut saved.source {
    source.channel = StoreChannel::Nightly;
  }
  assert!(has_update(&applet, &saved));
  saved = installed("0.9.0");
  assert!(has_update(&applet, &saved));
  applet.latest_stable_release = None;
  assert!(!has_update(&applet, &saved));
  applet.latest_release = release("3.0.0");
  applet.latest_stable_release = Some(applet.latest_release.clone());
  saved = installed("2.0.0-beta.1");
  if let Some(PackageSource::Store(source)) = &mut saved.source {
    source.channel = StoreChannel::Nightly;
  }
  assert!(!has_update(&applet, &saved));
}

#[test]
fn same_store_channel_switch_requires_the_reviewed_snapshot() {
  let mut report = empty_report();
  let saved = installed("1.0.0");
  report.applets.push(saved.clone());
  let review = review_install(&report, "https://store.example", "sample");
  let next = release("2.0.0-nightly.1");
  let hash = Some(saved.sha256.as_str());
  let snapshot = review.channel_snapshot.as_ref().unwrap();
  assert_eq!(
    release_action(&next, &review).0,
    ReleaseAction::SwitchChannel
  );
  assert!(checked_switch(&next, &review, hash, None, None).is_err());
  assert!(checked_switch(&next, &review, hash, Some(snapshot), None).is_err());
  assert_eq!(
    checked_switch(&next, &review, hash, None, Some(snapshot)).unwrap(),
    ReleaseAction::SwitchChannel
  );
  assert!(checked_switch(
    &release("1.0.0-nightly.1"),
    &review,
    hash,
    None,
    Some(snapshot)
  )
  .is_err());

  let retained = StoreSwitchSnapshot {
    removed: true,
    ..snapshot.clone()
  };
  let stale = retained.clone();
  report.applets.clear();
  report.retained.insert("sample".into(), retained);
  let review = review_install(&report, "https://store.example", "sample");
  assert!(checked_switch(
    &next,
    &review,
    None,
    None,
    Some(&StoreSwitchSnapshot {
      removed: false,
      ..stale.clone()
    })
  )
  .is_err());
  assert_eq!(
    checked_switch(&next, &review, None, None, Some(&stale)).unwrap(),
    ReleaseAction::SwitchChannel
  );
}

#[test]
fn installed_source_path_updates_or_switches_only_within_an_active_applets_store() {
  let mut report = empty_report();
  let mut saved = installed("1.0.0");
  let original = "https://original.example";
  let source = StoreSource {
    store_url: original.into(),
    applet_id: "sample".into(),
    channel: StoreChannel::Stable,
  };
  saved.source = Some(source.clone().into());
  report.applets.push(saved.clone());
  let mut request = InstallQuery {
    store_url: original.into(),
    applet_id: "sample".into(),
    installed_source: true,
    release: release("2.0.0-nightly.1"),
    expected_installed_sha256: Some(saved.sha256.clone()),
    switch_snapshot: None,
    channel_snapshot: None,
    operation_id: String::new(),
  };

  assert!(review_requested_install(&request, &report, original).is_err());
  request.channel_snapshot = review_install(&report, original, "sample").channel_snapshot;
  assert_eq!(
    review_requested_install(&request, &report, original).unwrap(),
    ReleaseAction::SwitchChannel
  );
  assert!(review_requested_install(&request, &report, "https://other.example").is_err());
  assert!(authorize_installed_source(Some(&saved), original, "neighbor").is_err());
  report.applets[0].sha256 = "b".repeat(64);
  assert!(review_requested_install(&request, &report, original).is_err());
  report.applets[0].sha256.clone_from(&saved.sha256);

  request.release = release("2.0.0");
  assert!(review_requested_install(&request, &report, original).is_err());
  request.channel_snapshot = None;
  assert_eq!(
    review_requested_install(&request, &report, original).unwrap(),
    ReleaseAction::Update
  );
  assert!(review_requested_install(&request, &report, "https://other.example").is_err());
  report.applets[0].sha256 = "b".repeat(64);
  assert!(review_requested_install(&request, &report, original).is_err());
  report.applets[0].sha256.clone_from(&saved.sha256);
  report.applets[0].source = Some(
    StoreSource {
      store_url: "https://other.example".into(),
      ..source
    }
    .into(),
  );
  assert!(review_requested_install(&request, &report, original).is_err());
  report.applets[0].source.clone_from(&saved.source);

  for version in ["0.9.0", "1.0.0"] {
    request.release = release(version);
    assert!(review_requested_install(&request, &report, original).is_err());
  }
  request.release = release("2.0.0");

  report.applets.clear();
  report.retained.insert(
    "sample".into(),
    StoreSwitchSnapshot {
      sha256: saved.sha256,
      source: saved.source,
      removed: true,
    },
  );
  let review = review_install(&report, original, "sample");
  assert!(authorize_installed_source(review.installed, original, "sample").is_err());
  assert!(review_requested_install(&request, &report, original).is_err());
}

#[test]
fn local_store_settings_are_atomic_optional_and_preserve_corrupt_files() {
  let root = tempfile::tempdir().unwrap();
  assert_eq!(
    state::load(root.path()).unwrap().store_url.as_deref(),
    DEFAULT_STORE_URL
  );
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

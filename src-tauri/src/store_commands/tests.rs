use super::{
  updates::{cache_projection, check_updates, read_cache, write_cache, SavedUpdate, SavedUpdates},
  *,
};
use apell_core::{
  bundles::{GitHubSource, Manifest, UnavailableApplet},
  store_client::StoreCatalog,
};

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

pub(super) fn listing(id: &str, stable: &str, nightly: Option<&str>) -> StoreApplet {
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

fn hint(applet: &InstalledApplet, version: &str) -> SavedUpdate {
  let Some(PackageSource::Store(source)) = applet.source.clone() else {
    panic!("fixture must be from a Store");
  };
  SavedUpdate {
    source,
    sha256: applet.sha256.clone(),
    version: version.into(),
  }
}

#[test]
fn saved_catalog_recomputes_installation_flags_without_claiming_updates() {
  let origin = "https://store.example";
  let ready = store_installation("ready", origin, "1.0.0", StoreChannel::Stable);
  let foreign = store_installation(
    "foreign",
    "https://other.example",
    "1.0.0",
    StoreChannel::Stable,
  );
  let mut installed = empty_report();
  installed.applets = vec![ready, foreign];
  installed.unavailable.push(UnavailableApplet {
    applet_id: "broken".into(),
    error: "Broken package".into(),
    can_remove: false,
    restore_snapshot: None,
  });
  let catalog = StoreCatalog {
    applets: ["ready", "foreign", "broken"]
      .map(|id| listing(id, "2.0.0", None))
      .into(),
  };
  let fresh = catalog_result(origin.into(), catalog.clone(), &installed, true);
  assert!(fresh.applets[0].update_available);
  let saved = catalog_result(origin.into(), catalog, &installed, false);
  assert!(saved.applets.iter().all(|entry| !entry.update_available));
  assert_eq!(
    saved.applets[0]
      .installed
      .as_ref()
      .unwrap()
      .manifest
      .version,
    "1.0.0"
  );
  assert!(!saved.applets[0].source_conflict);
  assert!(saved.applets[1].source_conflict && !saved.applets[1].installation_unavailable);
  assert!(saved.applets[2].source_conflict && saved.applets[2].installation_unavailable);
}

#[test]
fn saved_detail_is_public_and_never_accepts_pending_download_privileges() {
  let mut request =
    json!({ "store_url":"https://store.example", "applet_id":"sample", "installed_source":true });
  assert!(
    input::<SavedDetailQuery>(request.clone())
      .unwrap()
      .installed_source
  );
  request["pending_download"] = json!({});
  assert!(input::<SavedDetailQuery>(request).is_err());
  let applet = listing("sample", "2.0.0", None);
  let schema = output(SavedDetailResult {
    store_url: "https://store.example".into(),
    detail: StoreDetail {
      releases: vec![applet.latest_release.clone()],
      applet,
    },
  })
  .unwrap();
  assert_eq!(schema.as_object().unwrap().len(), 3);
  assert!(schema.get("applet").is_some() && schema.get("store_url").is_some());
  assert!(schema["releases"][0].get("action").is_none());
  assert!(schema["releases"][0].get("downloaded_bytes").is_none());
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
    can_remove: false,
    restore_snapshot: None,
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
  let result = check_updates(
    || std::future::ready(Ok(report.clone())),
    |origin| {
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
    },
    &SavedUpdates::new(),
  )
  .await
  .unwrap();

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
  let mut nightly = store_installation(
    "nightly",
    unavailable,
    "1.0.0-beta.1",
    StoreChannel::Nightly,
  );
  nightly.sha256 = "b".repeat(64);
  report.applets = vec![
    store_installation("sample", healthy, "1.0.0", StoreChannel::Stable),
    store_installation("other", unavailable, "1.0.0", StoreChannel::Stable),
    nightly,
  ];
  let saved = SavedUpdates::from([
    ("other".into(), hint(&report.applets[1], "2.0.0")),
    ("nightly".into(), hint(&report.applets[2], "2.0.0")),
  ]);
  let result = check_updates(
    || std::future::ready(Ok(report.clone())),
    |origin| async move {
      match origin.as_str() {
        "https://healthy.example" => Ok(StoreCatalog {
          applets: vec![listing("sample", "2.0.0", None)],
        }),
        "https://unavailable.example" => Err("Could not reach the store.".into()),
        _ => unreachable!(),
      }
    },
    &saved,
  )
  .await
  .unwrap();
  let json = output(result).unwrap();
  let sources = json["sources"].as_array().unwrap();
  assert_eq!(sources.len(), 2);
  assert_eq!(sources[0]["checked_count"], 1);
  assert_eq!(sources[0]["updates"][0]["version"], "2.0.0");
  assert!(sources[0]["updates"][0].get("release").is_none());
  assert!(sources[0].get("error").is_none());
  assert!(sources[0]["saved_updates"].as_array().unwrap().is_empty());
  assert_eq!(sources[1]["store_url"], unavailable);
  assert_eq!(sources[1]["installed_count"], 2);
  assert_eq!(sources[1]["checked_count"], 0);
  assert_eq!(sources[1]["error"], "Could not reach the store.");
  assert!(sources[1]["updates"].as_array().unwrap().is_empty());
  assert!(sources[1].get("unreachable_installations").is_none());
  let saved = sources[1]["saved_updates"].as_array().unwrap();
  assert_eq!(saved.len(), 1);
  assert_eq!(saved[0]["applet"]["applet_id"], "other");
  assert_eq!(saved[0]["version"], "2.0.0");
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
  let result = check_updates(
    || std::future::ready(Ok(report.clone())),
    |_| async {
      Ok(StoreCatalog {
        applets: vec![
          listing("nightly", "3.0.0", Some("2.0.0-beta.1")),
          listing("stable", "1.0.0", Some("2.0.0-beta.1")),
          listing("missing-channel", "3.0.0", None),
        ],
      })
    },
    &SavedUpdates::new(),
  )
  .await
  .unwrap();
  let source = &result.sources[0];
  assert_eq!(source.installed_count, 4);
  assert_eq!(source.checked_count, 2);
  assert!(source.saved_updates.is_empty());
  assert_eq!(source.updates.len(), 1);
  let update = &source.updates[0];
  assert_eq!(update.applet.manifest.name, "Sample");
  assert_eq!(
    update.applet.icon_data_url.as_deref(),
    Some("data:image/png;base64,aW5zdGFsbGVk")
  );
  assert_eq!(update.version, "2.0.0-beta.1");
}

#[tokio::test]
async fn update_cache_revalidates_installations_changed_during_the_network_check() {
  let available = "https://a.example";
  let offline = "https://b.example";
  let foreign = "https://other.example";
  let stable = |id, origin| store_installation(id, origin, "1.0.0", StoreChannel::Stable);
  let mut initial = empty_report();
  initial.applets = vec![
    stable("already-updated", available),
    stable("missing", available),
    stable("removed", offline),
    stable("replaced", offline),
    stable("channel", offline),
    stable("moved", offline),
    stable("match", offline),
  ];
  let earlier = initial
    .applets
    .iter()
    .map(|applet| (applet.manifest.applet_id.clone(), hint(applet, "2.0.0")))
    .collect();
  let mut current = initial.clone();
  current.applets[0].manifest.version = "2.0.0".into();
  current.applets[3].sha256 = "b".repeat(64);
  let Some(PackageSource::Store(channel)) = current.applets[4].source.as_mut() else {
    unreachable!()
  };
  channel.channel = StoreChannel::Nightly;
  let Some(PackageSource::Store(moved)) = current.applets[5].source.as_mut() else {
    unreachable!()
  };
  moved.store_url = foreign.into();
  current.applets.remove(2);
  let received = std::cell::Cell::new(false);
  let result = check_updates(
    || {
      std::future::ready(Ok(if received.get() {
        current.clone()
      } else {
        initial.clone()
      }))
    },
    |origin| {
      received.set(true);
      std::future::ready(match origin.as_str() {
        "https://a.example" => Ok(StoreCatalog {
          applets: vec![listing("already-updated", "2.0.0", None)],
        }),
        "https://b.example" => Err("Offline".into()),
        _ => panic!("new source was not present when we began"),
      })
    },
    &earlier,
  )
  .await
  .unwrap();
  let [online, offline, moved] = &result.sources[..] else {
    panic!("expected all three current origins")
  };
  assert_eq!((online.installed_count, online.checked_count), (2, 1));
  assert!(online.updates.is_empty() && online.saved_updates.is_empty());
  assert_eq!((offline.installed_count, offline.checked_count), (3, 0));
  assert_eq!(offline.saved_updates.len(), 1);
  assert_eq!(offline.saved_updates[0].applet.manifest.applet_id, "match");
  assert_eq!(moved.store_url, foreign);
  assert!(moved.saved_updates.is_empty());
  assert_eq!(
    cache_projection(&result).into_keys().collect::<Vec<_>>(),
    ["match"]
  );
}

#[tokio::test]
async fn update_cache_stores_only_binding_and_version_and_ignores_invalid_local_data() {
  let root = tempfile::tempdir().unwrap();
  assert!(read_cache(root.path()).is_empty());
  let origin = "https://store.example";
  let mut report = empty_report();
  report.applets.push(store_installation(
    "sample",
    origin,
    "1.0.0",
    StoreChannel::Stable,
  ));
  report.applets[0].icon_data_url = Some("data:image/png;base64,fixture".into());
  let result = check_updates(
    || std::future::ready(Ok(report.clone())),
    |_| async {
      Ok(StoreCatalog {
        applets: vec![listing("sample", "2.0.0", None)],
      })
    },
    &SavedUpdates::new(),
  )
  .await
  .unwrap();
  let cache = cache_projection(&result);
  write_cache(root.path(), &cache);
  assert_eq!(read_cache(root.path()), cache);
  let path = root.path().join("store-updates-v1.json");
  let bytes = std::fs::read(&path).unwrap();
  assert_eq!(
    serde_json::from_slice::<Value>(&bytes).unwrap(),
    json!([{
      "source":{"store_url":origin,"applet_id":"sample","channel":"stable"},
      "sha256":"a".repeat(64), "version":"2.0.0",
    }])
  );
  std::fs::write(&path, b"invalid cache").unwrap();
  assert!(read_cache(root.path()).is_empty());
  std::fs::write(&path, vec![b'x'; 1024 * 1024 + 1]).unwrap();
  assert!(read_cache(root.path()).is_empty());
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
    can_remove: true,
    restore_snapshot: None,
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
    resume_download: false,
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

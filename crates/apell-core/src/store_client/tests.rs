mod fixture;

use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use fixture::{applet, detail, package, release, MockStore, Reply};

#[test]
fn store_addresses_are_root_origins_without_credentials() {
  assert_eq!(
    normalize_store_url("https://STORE.example:443/").unwrap(),
    "https://store.example"
  );
  assert_eq!(
    normalize_store_url("http://localhost:3000/").unwrap(),
    "http://localhost:3000"
  );
  assert!(normalize_store_url("http://[::1]:3000").is_ok());
  for invalid in [
    "https://user:secret@store.example",
    "https://@store.example",
    "https://store.example/path",
    "https://store.example/path/..",
    "https://store.example/?token=secret",
    "https://store.example/#part",
    "http://store.example",
    "https:store.example",
    "file:///tmp/store",
    "https://store.example\n",
    "https://store.example\\path",
  ] {
    assert!(normalize_store_url(invalid).is_err(), "{invalid}");
  }
}

#[test]
fn release_and_catalog_metadata_are_validated_before_use() {
  let valid = release(&package("sample", "1.0.0", &["sqlite"]), "1.0.0");
  valid.validate("sample").unwrap();
  let mutations: [fn(&mut StoreRelease); 8] = [
    |r| r.download_url = "https://other.example/package.apel".into(),
    |r| r.download_url = "//other.example/package.apel".into(),
    |r| r.sha256 = "A".repeat(64),
    |r| r.size_bytes = 25 * 1024 * 1024,
    |r| r.api_version = 999,
    |r| r.permissions = vec!["unknown".into()],
    |r| r.prerelease = true,
    |r| r.version = "../escape".into(),
  ];
  for mutate in mutations {
    let mut r = valid.clone();
    mutate(&mut r);
    assert!(r.validate("sample").is_err());
  }
  assert!(valid.validate("../sample").is_err());
  let mut bad_icon = applet(&valid);
  bad_icon.icon_url = Some("https://other.example/icon.png".into());
  assert!(bad_icon.validate().is_err());
  let repeated = StoreCatalog {
    applets: vec![applet(&valid); 2],
  };
  assert!(repeated.validate().is_err());
  let huge = StoreCatalog {
    applets: vec![applet(&valid); 257],
  };
  assert!(huge.validate().unwrap_err().contains("too large"));
  let mut inconsistent = detail(&valid);
  inconsistent.releases.clear();
  assert!(inconsistent.validate("sample").is_err());
  assert!(detail(&valid).validate("different").is_err());
}

#[tokio::test]
async fn nightly_head_is_independent_of_stable_and_recovers_from_legacy_details() {
  let stable = release(&package("sample", "2.0.0", &["sqlite"]), "2.0.0");
  let nightly = release(
    &package("sample", "1.5.0-nightly.10", &["sqlite"]),
    "1.5.0-nightly.10",
  );
  let older = release(&package("sample", "1.4.0-rc.1", &["sqlite"]), "1.4.0-rc.1");
  let mut metadata = detail(&stable);
  metadata.releases.extend([older.clone(), nightly.clone()]);
  metadata.applet.latest_nightly_release = Some(older);
  assert!(metadata.validate("sample").is_err());
  metadata.applet.latest_nightly_release = Some(stable);
  assert!(metadata.applet.validate().is_err());
  metadata.applet.latest_nightly_release = Some(nightly.clone());
  metadata.validate("sample").unwrap();

  let mut legacy = serde_json::to_value(&metadata).unwrap();
  legacy["applet"]
    .as_object_mut()
    .unwrap()
    .remove("latest_nightly_release");
  let server = MockStore::new(vec![(
    "/api/v1/applets/sample".into(),
    Reply::json(&legacy),
  )])
  .await;
  let recovered = StoreClient::new(&server.origin)
    .unwrap()
    .detail("sample")
    .await
    .unwrap();
  assert_eq!(recovered.applet.nightly_release(), Some(&nightly));
}

#[tokio::test]
async fn catalog_networking_rejects_redirects_and_malformed_responses() {
  for reply in [
    Reply::status(302, b"".to_vec()).header("Location", "https://other.example/api/v1/applets"),
    Reply::ok(b"not JSON".to_vec()),
    Reply::ok(br#"{"applets":{}}"#.to_vec()),
    Reply::ok(b"{}".to_vec()).header("Content-Length", "8388609"),
  ] {
    let server = MockStore::new(vec![("/api/v1/applets".into(), reply)]).await;
    assert!(StoreClient::new(&server.origin)
      .unwrap()
      .catalog()
      .await
      .is_err());
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = requests[0].to_lowercase();
    assert!(
      !request.contains("authorization:")
        && !request.contains("cookie:")
        && !request.contains("proxy-authorization:")
    );
  }
}

#[tokio::test]
async fn a_custom_tls_builder_still_rejects_store_redirects() {
  let server = MockStore::new(vec![
    (
      "/api/v1/applets".into(),
      Reply::status(302, Vec::new()).header("Location", "/redirected"),
    ),
    (
      "/redirected".into(),
      Reply::json(&StoreCatalog { applets: vec![] }),
    ),
  ])
  .await;
  let builder = Client::builder().redirect(reqwest::redirect::Policy::limited(5));
  let error = StoreClient::with_http_builder(&server.origin, builder)
    .unwrap()
    .catalog()
    .await
    .unwrap_err();
  assert!(error.contains("The store returned a redirect"));
  assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn network_metadata_cannot_redirect_downloads_or_icons() {
  let release = release(&package("sample", "1.0.0", &["sqlite"]), "1.0.0");
  let mut cross_origin = detail(&release);
  cross_origin.releases[0].download_url = "https://other.example/package.apel".into();
  cross_origin.applet.latest_release = cross_origin.releases[0].clone();
  cross_origin.applet.latest_stable_release = Some(cross_origin.releases[0].clone());
  let mut wrong_id = detail(&release);
  wrong_id.applet.applet_id = "other".into();
  for bad in [cross_origin, wrong_id] {
    let server = MockStore::new(vec![("/api/v1/applets/sample".into(), Reply::json(&bad))]).await;
    let client = StoreClient::new(&server.origin).unwrap();
    assert!(client.detail("sample").await.is_err());
    assert!(client.icon("https://other.example/icon.png").await.is_err());
    assert_eq!(server.requests.lock().unwrap().len(), 1);
  }
}

#[tokio::test]
async fn downloads_verify_raw_bytes_and_report_progress() {
  let bytes = package("sample", "1.0.0", &["sqlite"]);
  let selected = release(&bytes, "1.0.0");
  let server = MockStore::release(&selected, bytes.clone()).await;
  let progress = std::sync::Mutex::new(Vec::new());
  let downloaded = StoreClient::new(&server.origin)
    .unwrap()
    .download("sample", &selected, |received, total| {
      progress.lock().unwrap().push((received, total))
    })
    .await
    .unwrap();
  assert_eq!(downloaded, bytes);
  assert_eq!(
    progress.lock().unwrap().last(),
    Some(&(selected.size_bytes, Some(selected.size_bytes)))
  );
  let mut damaged = bytes;
  damaged[0] ^= 1;
  let server = MockStore::release(&selected, damaged).await;
  assert!(StoreClient::new(&server.origin)
    .unwrap()
    .download("sample", &selected, |_, _| {})
    .await
    .unwrap_err()
    .contains("does not match"));
}

#[tokio::test]
async fn interrupted_download_resumes_the_reviewed_range_and_rejects_bad_ranges() {
  let bytes = package("sample", "1.0.0", &["sqlite"]);
  let selected = release(&bytes, "1.0.0");
  let size = bytes.len();
  let middle = size / 2;
  let etag = format!("\"{}\"", selected.sha256);
  let path = &selected.download_url;
  let server = MockStore::new(vec![
    (
      "/api/v1/applets/sample".into(),
      Reply::json(&detail(&selected)),
    ),
    (
      path.clone(),
      Reply::ok(bytes[..middle].to_vec()).header("Content-Length", &size.to_string()),
    ),
    (
      path.clone(),
      Reply::status(206, bytes[middle..].to_vec())
        .header(
          "Content-Range",
          &format!("bytes {middle}-{}/{size}", size - 1),
        )
        .header("ETag", "\"different\""),
    ),
    (
      path.clone(),
      Reply::status(206, bytes[middle..].to_vec())
        .header(
          "Content-Range",
          &format!("bytes {middle}-{}/{size}", size - 1),
        )
        .header("ETag", &etag),
    ),
  ])
  .await;
  let client = StoreClient::new(&server.origin).unwrap();
  let cache = tempfile::tempdir().unwrap();
  let checkpoint = DownloadCheckpoint::new(cache.path(), &server.origin, &selected);
  let result = client
    .download_resumable(
      "sample",
      &selected,
      &checkpoint,
      std::future::pending(),
      |_, _| {},
    )
    .await
    .unwrap();
  assert!(
    matches!(result, DownloadOutcome::Paused { received_bytes } if received_bytes == middle as u64)
  );
  assert!(client
    .download_resumable(
      "sample",
      &selected,
      &checkpoint,
      std::future::pending(),
      |_, _| {}
    )
    .await
    .err()
    .unwrap()
    .contains("invalid download range"));
  assert_eq!(checkpoint.received(size as u64).await, middle as u64);
  let result = client
    .download_resumable(
      "sample",
      &selected,
      &checkpoint,
      std::future::pending(),
      |_, _| {},
    )
    .await
    .unwrap();
  assert!(matches!(result, DownloadOutcome::Verified(downloaded) if downloaded == bytes));
  assert_eq!(checkpoint.received(size as u64).await, 0);
  let requests = server.requests.lock().unwrap();
  assert!(requests
    .last()
    .unwrap()
    .contains(&format!("range: bytes={middle}-")));
  assert!(requests
    .last()
    .unwrap()
    .contains(&format!("if-range: {etag}")));
}

#[tokio::test]
async fn expired_background_time_keeps_bytes_and_old_stores_restart_the_body() {
  let bytes = package("sample", "1.0.0", &["sqlite"]);
  let selected = release(&bytes, "1.0.0");
  let server = MockStore::release(&selected, bytes.clone()).await;
  let client = StoreClient::new(&server.origin).unwrap();
  let cache = tempfile::tempdir().unwrap();
  let checkpoint = DownloadCheckpoint::new(cache.path(), &server.origin, &selected);
  let pause = tokio::sync::Notify::new();
  let result = client
    .download_resumable(
      "sample",
      &selected,
      &checkpoint,
      pause.notified(),
      |received, _| {
        if received == 0 && server.requests.lock().unwrap().len() == 2 {
          pause.notify_one();
        }
      },
    )
    .await
    .unwrap();
  assert!(matches!(
    result,
    DownloadOutcome::Paused { received_bytes: 0 }
  ));
  // Simulate a checkpoint written by an earlier interrupted request.
  checkpoint.prepare_for_test(&bytes[..8]).await;
  let result = client
    .download_resumable(
      "sample",
      &selected,
      &checkpoint,
      std::future::pending(),
      |_, _| {},
    )
    .await
    .unwrap();
  assert!(matches!(result, DownloadOutcome::Verified(downloaded) if downloaded == bytes));
  assert!(server
    .requests
    .lock()
    .unwrap()
    .last()
    .unwrap()
    .contains("range: bytes=8-"));
}

#[tokio::test]
async fn selected_snapshot_must_remain_available_and_unchanged() {
  let bytes = package("sample", "1.0.0", &["sqlite"]);
  let selected = release(&bytes, "1.0.0");
  let mut changed = selected.clone();
  changed.permissions.clear();
  let mut absent = selected.clone();
  absent.version = "2.0.0".into();
  absent.download_url = "/api/v1/applets/sample/versions/2.0.0/download".into();
  for current in [changed, absent] {
    let server = MockStore::release(&current, bytes.clone()).await;
    let error = StoreClient::new(&server.origin)
      .unwrap()
      .download("sample", &selected, |_, _| {})
      .await
      .unwrap_err();
    assert!(error.contains("changed") || error.contains("no longer available"));
    assert_eq!(server.requests.lock().unwrap().len(), 1);
  }
}

#[tokio::test]
async fn package_manifest_must_match_reviewed_metadata_even_with_valid_hash() {
  for bytes in [
    package("other", "1.0.0", &["sqlite"]),
    package("sample", "2.0.0", &["sqlite"]),
    package("sample", "1.0.0", &["llm"]),
  ] {
    let selected = release(&bytes, "1.0.0");
    let server = MockStore::release(&selected, bytes).await;
    let error = StoreClient::new(&server.origin)
      .unwrap()
      .download("sample", &selected, |_, _| {})
      .await
      .unwrap_err();
    assert!(error.contains("identity or permissions"), "{error}");
  }
}

#[tokio::test]
async fn package_redirects_and_oversized_downloads_are_rejected() {
  let selected = release(&package("sample", "1.0.0", &["sqlite"]), "1.0.0");
  for reply in [
    Reply::status(307, Vec::new()).header("Location", "https://other.example/package.apel"),
    Reply::ok(vec![0; selected.size_bytes as usize + 1]),
  ] {
    let server = MockStore::new(vec![
      (
        "/api/v1/applets/sample".into(),
        Reply::json(&detail(&selected)),
      ),
      (selected.download_url.clone(), reply),
    ])
    .await;
    assert!(StoreClient::new(&server.origin)
      .unwrap()
      .download("sample", &selected, |_, _| {})
      .await
      .is_err());
    assert_eq!(server.requests.lock().unwrap().len(), 2);
  }
}

#[tokio::test]
async fn icons_require_matching_png_digest_and_bounded_dimensions() {
  let png = crate::icons::tests::png(1, 1, 255);
  let path = format!("/media/{}.png", digest(&png));
  let server = MockStore::new(vec![(path.clone(), Reply::ok(png.clone()))]).await;
  let uri = StoreClient::new(&server.origin)
    .unwrap()
    .icon(&path)
    .await
    .unwrap();
  assert_eq!(
    uri,
    format!("data:image/png;base64,{}", STANDARD.encode(&png))
  );
  let mut oversized = png.clone();
  oversized[16..20].copy_from_slice(&1025u32.to_be_bytes());
  for bad in [b"<html>not an image</html>".to_vec(), oversized] {
    let path = format!("/media/{}.png", digest(&bad));
    let server = MockStore::new(vec![(path.clone(), Reply::ok(bad))]).await;
    assert!(StoreClient::new(&server.origin)
      .unwrap()
      .icon(&path)
      .await
      .is_err());
  }
  let server = MockStore::new(vec![(path.clone(), Reply::ok(vec![0; png.len()]))]).await;
  assert!(StoreClient::new(&server.origin)
    .unwrap()
    .icon(&path)
    .await
    .is_err());
}

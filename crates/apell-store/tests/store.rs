use apell_store::{router, AppletDetail, Catalog, Config, PublishMetadata, PublishResult, Store};
use axum::{
  body::Body,
  http::{header, Request, StatusCode},
  Router,
};
use http_body_util::BodyExt;
use std::{
  io::{Cursor, Read, Write},
  sync::{Arc, Barrier},
};
use tower::ServiceExt;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

const TOKEN: &str = "fixture-token-with-at-least-thirty-two-characters";

fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
  let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
  for (path, bytes) in entries {
    zip
      .start_file(
        *path,
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
      )
      .unwrap();
    zip.write_all(bytes).unwrap();
  }
  zip.finish().unwrap().into_inner()
}

fn package(version: &str, text: &str) -> Vec<u8> {
  let manifest = serde_json::to_vec(&serde_json::json!({
    "schema_version":1,"api_version":2,"applet_id":"sample","name":"Sample",
    "version":version,"permissions":["sqlite"],"network":false,
  }))
  .unwrap();
  archive(&[
    ("manifest.json", &manifest),
    ("index.html", text.as_bytes()),
  ])
}

fn metadata() -> PublishMetadata {
  PublishMetadata {
    description: "A sample applet.".into(),
    project_url: Some("https://example.org/project".into()),
    release_notes: "First line.\nSecond line.".into(),
  }
}

fn app() -> (tempfile::TempDir, Router) {
  let root = tempfile::tempdir().unwrap();
  std::fs::create_dir(root.path().join("web")).unwrap();
  std::fs::write(
    root.path().join("web/index.html"),
    "<html>Store frontend</html>",
  )
  .unwrap();
  let config = Config {
    listen: "127.0.0.1:0".parse().unwrap(),
    data_dir: root.path().join("data"),
    web_dir: root.path().join("web"),
    publish_token: TOKEN.into(),
    oidc: None,
    oauth: None,
    account: None,
    config_sync: None,
    bootstrap_owner: None,
  };
  let app = router(&config).unwrap();
  (root, app)
}

fn multipart(bytes: &[u8], metadata: &PublishMetadata, icon: Option<&[u8]>) -> Vec<u8> {
  let mut body = Vec::new();
  body.extend_from_slice(b"--fixture-boundary\r\nContent-Disposition: form-data; name=\"metadata\"\r\nContent-Type: application/json\r\n\r\n");
  body.extend_from_slice(&serde_json::to_vec(metadata).unwrap());
  body.extend_from_slice(b"\r\n--fixture-boundary\r\nContent-Disposition: form-data; name=\"package\"; filename=\"sample.apel\"\r\nContent-Type: application/octet-stream\r\n\r\n");
  body.extend_from_slice(bytes);
  if let Some(icon) = icon {
    body.extend_from_slice(b"\r\n--fixture-boundary\r\nContent-Disposition: form-data; name=\"icon\"; filename=\"icon.png\"\r\nContent-Type: image/png\r\n\r\n");
    body.extend_from_slice(icon);
  }
  body.extend_from_slice(b"\r\n--fixture-boundary--\r\n");
  body
}

fn publish_request(bytes: &[u8]) -> Request<Body> {
  Request::post("/api/v1/publish")
    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
    .header(
      header::CONTENT_TYPE,
      "multipart/form-data; boundary=fixture-boundary",
    )
    .body(Body::from(multipart(bytes, &metadata(), None)))
    .unwrap()
}

#[tokio::test]
async fn authentication_rejects_before_polling_body_and_writes_nothing() {
  let (root, app) = app();
  let body = Body::from_stream(futures_util::stream::poll_fn(
    |_| -> std::task::Poll<Option<Result<Vec<u8>, std::io::Error>>> {
      panic!("Unauthenticated request body must not be polled");
    },
  ));
  let response = app
    .oneshot(Request::post("/api/v1/publish").body(body).unwrap())
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
  assert_eq!(
    std::fs::read_dir(root.path().join("data/packages"))
      .unwrap()
      .count(),
    0
  );
  let error: serde_json::Value =
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
  assert_eq!(error["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn publish_catalog_download_and_conditional_request_keep_identity() {
  let (_root, app) = app();
  let package = package("1.2.3", "<h1>Sample</h1>");
  let response = app
    .clone()
    .oneshot(publish_request(&package))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::CREATED);
  let result: PublishResult =
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
  assert_eq!(result.sha256, apell_package::digest(&package));
  let response = app
    .clone()
    .oneshot(Request::get("/api/v1/applets").body(Body::empty()).unwrap())
    .await
    .unwrap();
  let catalog: Catalog =
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
  assert_eq!(catalog.applets.len(), 1);
  let release = &catalog.applets[0].latest_release;
  assert_eq!(release.sha256, result.sha256);
  assert_eq!(release.size_bytes, package.len() as u64);
  assert_eq!(release.permissions, ["sqlite"]);
  assert!(!release.network);
  assert!(!release.has_backend);
  assert_eq!(release.api_version, 2);
  let response = app
    .clone()
    .oneshot(
      Request::get(&release.download_url)
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  assert_eq!(
    response.headers()[header::CONTENT_TYPE],
    "application/octet-stream"
  );
  assert_eq!(
    response.headers()[header::CONTENT_DISPOSITION],
    "attachment; filename=\"sample-1.2.3.apel\""
  );
  assert_eq!(
    response.headers()[header::X_CONTENT_TYPE_OPTIONS],
    "nosniff"
  );
  let etag = response.headers()[header::ETAG].clone();
  assert_eq!(
    response.into_body().collect().await.unwrap().to_bytes(),
    package
  );
  let response = app
    .oneshot(
      Request::get(&release.download_url)
        .header(header::IF_NONE_MATCH, etag)
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
  assert!(response
    .into_body()
    .collect()
    .await
    .unwrap()
    .to_bytes()
    .is_empty());
}

#[tokio::test]
async fn package_download_resumes_only_the_matching_representation() {
  let (_root, app) = app();
  let bytes = package("1.2.3", "<h1>Sample</h1>");
  let response = app.clone().oneshot(publish_request(&bytes)).await.unwrap();
  assert_eq!(response.status(), StatusCode::CREATED);
  let url = "/api/v1/applets/sample/versions/1.2.3/download";
  let size = bytes.len();
  let etag = format!("\"{}\"", apell_package::digest(&bytes));
  let head = app
    .clone()
    .oneshot(
      Request::head(url)
        .header(header::RANGE, "bytes=1-5")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(head.status(), StatusCode::OK);
  assert_eq!(head.headers()[header::CONTENT_LENGTH], size.to_string());
  assert_eq!(head.headers()[header::ACCEPT_RANGES], "bytes");
  assert!(!head.headers().contains_key(header::CONTENT_RANGE));
  assert!(head
    .into_body()
    .collect()
    .await
    .unwrap()
    .to_bytes()
    .is_empty());

  let offset = size / 2;
  let resumed = app
    .clone()
    .oneshot(
      Request::get(url)
        .header(header::RANGE, format!("bytes={offset}-"))
        .header(header::IF_RANGE, &etag)
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(resumed.status(), StatusCode::PARTIAL_CONTENT);
  assert_eq!(
    resumed.headers()[header::CONTENT_RANGE],
    format!("bytes {offset}-{}/{size}", size - 1)
  );
  assert_eq!(
    resumed.headers()[header::CONTENT_LENGTH],
    (size - offset).to_string()
  );
  assert_eq!(resumed.headers()[header::ETAG], etag);
  let mut combined = bytes[..offset].to_vec();
  combined.extend_from_slice(&resumed.into_body().collect().await.unwrap().to_bytes());
  assert_eq!(combined, bytes);

  for validator in ["\"stale\"".to_owned(), format!("W/{etag}")] {
    let full = app
      .clone()
      .oneshot(
        Request::get(url)
          .header(header::RANGE, "bytes=1-5")
          .header(header::IF_RANGE, validator)
          .body(Body::empty())
          .unwrap(),
      )
      .await
      .unwrap();
    assert_eq!(full.status(), StatusCode::OK);
    assert!(!full.headers().contains_key(header::CONTENT_RANGE));
    assert_eq!(full.into_body().collect().await.unwrap().to_bytes(), bytes);
  }

  let suffix = app
    .clone()
    .oneshot(
      Request::get(url)
        .header(header::RANGE, "bytes=-8")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(suffix.status(), StatusCode::PARTIAL_CONTENT);
  assert_eq!(
    suffix.headers()[header::CONTENT_RANGE],
    format!("bytes {}-{}/{size}", size - 8, size - 1)
  );
  assert_eq!(
    suffix.into_body().collect().await.unwrap().to_bytes(),
    &bytes[size - 8..]
  );

  let out_of_bounds = app
    .clone()
    .oneshot(
      Request::get(url)
        .header(header::RANGE, format!("bytes={size}-"))
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(out_of_bounds.status(), StatusCode::RANGE_NOT_SATISFIABLE);
  assert_eq!(
    out_of_bounds.headers()[header::CONTENT_RANGE],
    format!("bytes */{size}")
  );
  assert!(out_of_bounds
    .into_body()
    .collect()
    .await
    .unwrap()
    .to_bytes()
    .is_empty());

  let unchanged = app
    .oneshot(
      Request::get(url)
        .header(header::RANGE, "bytes=1-5")
        .header(header::IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  assert_eq!(unchanged.status(), StatusCode::NOT_MODIFIED);
  assert!(!unchanged.headers().contains_key(header::CONTENT_RANGE));
}

#[test]
fn repeated_uploads_are_idempotent_and_versions_are_immutable() {
  let root = tempfile::tempdir().unwrap();
  let store = Store::open(root.path()).unwrap();
  let bytes = package("1.0.0", "first");
  assert!(
    store
      .publish(bytes.clone(), metadata(), None)
      .unwrap()
      .created
  );
  let mut changed_metadata = metadata();
  changed_metadata.description = "Different listing".into();
  assert_eq!(
    store
      .publish(bytes.clone(), changed_metadata, None)
      .unwrap_err()
      .status,
    StatusCode::CONFLICT
  );
  assert!(!store.publish(bytes, metadata(), None).unwrap().created);
  assert_eq!(
    store.detail("sample").unwrap().applet.description,
    "A sample applet."
  );
  let conflict = store
    .publish(package("1.0.0", "different"), metadata(), None)
    .unwrap_err();
  assert_eq!(conflict.status, StatusCode::CONFLICT);
  assert_eq!(store.detail("sample").unwrap().releases.len(), 1);
}

#[test]
fn concurrent_publishers_cannot_replace_a_version() {
  let root = tempfile::tempdir().unwrap();
  let store = Store::open(root.path()).unwrap();
  let barrier = Arc::new(Barrier::new(2));
  let handles: Vec<_> = ["first", "second"]
    .into_iter()
    .map(|text| {
      let store = store.clone();
      let barrier = barrier.clone();
      std::thread::spawn(move || {
        barrier.wait();
        store.publish(package("1.0.0", text), metadata(), None)
      })
    })
    .collect();
  let results: Vec<_> = handles
    .into_iter()
    .map(|handle| handle.join().unwrap())
    .collect();
  assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
  assert_eq!(
    results
      .iter()
      .filter(|result| result
        .as_ref()
        .is_err_and(|e| e.status == StatusCode::CONFLICT))
      .count(),
    1
  );
  assert_eq!(store.detail("sample").unwrap().releases.len(), 1);
}

#[test]
fn semver_order_and_stable_selection_are_independent_of_upload_order() {
  let root = tempfile::tempdir().unwrap();
  let store = Store::open(root.path()).unwrap();
  for version in [
    "1.9.0",
    "2.0.0-nightly.2",
    "1.10.0",
    "2.0.0-nightly.10",
    "1.0.0",
  ] {
    store
      .publish(package(version, "sample"), metadata(), None)
      .unwrap();
  }
  let detail = store.detail("sample").unwrap();
  assert_eq!(detail.applet.latest_release.version, "2.0.0-nightly.10");
  assert_eq!(
    detail.applet.latest_nightly_release.unwrap().version,
    "2.0.0-nightly.10"
  );
  assert_eq!(
    detail.applet.latest_stable_release.unwrap().version,
    "1.10.0"
  );
  assert_eq!(
    detail
      .releases
      .iter()
      .map(|release| release.version.as_str())
      .collect::<Vec<_>>(),
    [
      "2.0.0-nightly.10",
      "2.0.0-nightly.2",
      "1.10.0",
      "1.9.0",
      "1.0.0"
    ]
  );
  assert_eq!(
    store
      .publish(package("1.10.0+different", "sample"), metadata(), None)
      .unwrap_err()
      .status,
    StatusCode::CONFLICT
  );
}

#[test]
fn stable_publication_does_not_replace_the_nightly_head() {
  let root = tempfile::tempdir().unwrap();
  let store = Store::open(root.path()).unwrap();
  // Older previews are Nightly without changing their immutable version.
  for version in ["1.9.0-rc.3", "2.0.0"] {
    store
      .publish(package(version, "sample"), metadata(), None)
      .unwrap();
  }
  let applet = store.detail("sample").unwrap().applet;
  assert_eq!(applet.latest_nightly_release.unwrap().version, "1.9.0-rc.3");
  // A Stable version ahead of every nightly must never eclipse that channel.
  for version in ["1.10.0-nightly.10", "1.10.0-nightly.2"] {
    store
      .publish(package(version, "sample"), metadata(), None)
      .unwrap();
  }
  for applet in [
    store.catalog().unwrap().applets.remove(0),
    store.detail("sample").unwrap().applet,
  ] {
    assert_eq!(applet.latest_release.version, "2.0.0");
    assert_eq!(applet.latest_stable_release.unwrap().version, "2.0.0");
    assert_eq!(
      applet.latest_nightly_release.unwrap().version,
      "1.10.0-nightly.10"
    );
  }
}

#[test]
fn package_and_metadata_errors_do_not_publish_files() {
  let root = tempfile::tempdir().unwrap();
  let store = Store::open(root.path()).unwrap();
  for bytes in [
    b"not a zip".to_vec(),
    archive(&[("../escape", b"bad")]),
    vec![0; apell_package::MAX_BUNDLE_BYTES + 1],
  ] {
    assert_eq!(
      store.publish(bytes, metadata(), None).unwrap_err().code,
      "invalid_package"
    );
  }
  let mut invalid = metadata();
  invalid.project_url = Some("https://account:secret@example.org".into());
  assert!(store
    .publish(package("1.0.0", "sample"), invalid, None)
    .is_err());
  let mut invalid = metadata();
  invalid.description = "invisible\0data".into();
  assert!(store
    .publish(package("1.0.0", "sample"), invalid, None)
    .is_err());
  assert_eq!(
    std::fs::read_dir(root.path().join("packages"))
      .unwrap()
      .count(),
    0
  );
  assert!(store.catalog().unwrap().applets.is_empty());
}

#[test]
fn corrupted_package_is_never_downloaded_or_republished() {
  let root = tempfile::tempdir().unwrap();
  let store = Store::open(root.path()).unwrap();
  let bytes = package("1.0.0", "sample");
  let result = store.publish(bytes.clone(), metadata(), None).unwrap();
  std::fs::write(
    root
      .path()
      .join("packages")
      .join(format!("{}.apel", result.sha256)),
    b"corrupted",
  )
  .unwrap();
  assert_eq!(
    store.download("sample", "1.0.0").unwrap_err().code,
    "storage_error"
  );
  assert_eq!(
    store.publish(bytes, metadata(), None).unwrap_err().code,
    "storage_error"
  );
}

fn png(width: u32, height: u32) -> Vec<u8> {
  let mut bytes = Vec::new();
  {
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgba);
    let mut writer = encoder.write_header().unwrap();
    writer
      .write_image_data(&vec![128; (width * height * 4) as usize])
      .unwrap();
  }
  bytes
}

#[tokio::test]
async fn png_icons_are_validated_and_served_by_digest() {
  let (_root, app) = app();
  let icon = png(2, 2);
  let request = Request::post("/api/v1/publish")
    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
    .header(
      header::CONTENT_TYPE,
      "multipart/form-data; boundary=fixture-boundary",
    )
    .body(Body::from(multipart(
      &package("1.0.0", "sample"),
      &metadata(),
      Some(&icon),
    )))
    .unwrap();
  assert_eq!(
    app.clone().oneshot(request).await.unwrap().status(),
    StatusCode::CREATED
  );
  let response = app
    .clone()
    .oneshot(
      Request::get("/api/v1/applets/sample")
        .body(Body::empty())
        .unwrap(),
    )
    .await
    .unwrap();
  let detail: AppletDetail =
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
  let expected = format!("/media/{}.png", apell_package::digest(&icon));
  assert_eq!(detail.applet.icon_url.as_deref(), Some(expected.as_str()));
  let response = app
    .oneshot(Request::get(expected).body(Body::empty()).unwrap())
    .await
    .unwrap();
  assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
  assert_eq!(
    response.into_body().collect().await.unwrap().to_bytes(),
    icon
  );
}

#[test]
fn malformed_and_oversized_icons_are_rejected() {
  let root = tempfile::tempdir().unwrap();
  let store = Store::open(root.path()).unwrap();
  for icon in [b"<svg/>".to_vec(), png(1025, 1), vec![0; 256 * 1024 + 1]] {
    assert!(store
      .publish(package("1.0.0", "sample"), metadata(), Some(icon))
      .is_err());
  }
  assert!(store.catalog().unwrap().applets.is_empty());
}

#[tokio::test]
async fn frontend_deep_links_serve_html_for_get_and_head() {
  let (_root, app) = app();
  for route in [
    "/",
    "/account",
    "/account/",
    "/account/new",
    "/account/new/",
    "/account/cli?code=ABCDE-FGHJK",
    "/account/cli/?code=ABCDE-FGHJK",
    "/account/packages/sample",
    "/account/packages/sample/",
    "/applets/sample",
    "/applets/sample/",
  ] {
    for method in ["GET", "HEAD"] {
      let response = app
        .clone()
        .oneshot(
          Request::builder()
            .method(method)
            .uri(route)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
      assert_eq!(response.status(), StatusCode::OK, "{method} {route}");
      assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/html",
        "{method} {route}"
      );
      let body = response.into_body().collect().await.unwrap().to_bytes();
      if method == "HEAD" {
        assert!(body.is_empty(), "{method} {route}");
      } else {
        assert_eq!(
          body.as_ref(),
          b"<html>Store frontend</html>",
          "{method} {route}"
        );
      }
    }
  }
}

#[tokio::test]
async fn unknown_api_and_media_paths_do_not_return_the_website() {
  let (_root, app) = app();
  for route in [
    "/api",
    "/api/",
    "/api/no-such-endpoint",
    "/media",
    "/media/",
    "/media/not-an-icon.png",
    "/media/missing/icon.png",
  ] {
    for method in ["GET", "HEAD"] {
      let response = app
        .clone()
        .oneshot(
          Request::builder()
            .method(method)
            .uri(route)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
      assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {route}");
      assert!(
        response.headers()[header::CONTENT_TYPE]
          .to_str()
          .unwrap()
          .starts_with("application/json"),
        "{method} {route}: {:?}",
        response.headers()
      );
    }
  }
  for route in [
    "/assets/missing.js",
    "/missing.css",
    "/unknown",
    "/account/unknown",
    "/applets/sample/unknown",
  ] {
    for method in ["GET", "HEAD"] {
      let response = app
        .clone()
        .oneshot(
          Request::builder()
            .method(method)
            .uri(route)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
      assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {route}");
      let body = response.into_body().collect().await.unwrap().to_bytes();
      assert!(body.is_empty(), "{method} {route}");
    }
  }
  let response = app
    .oneshot(Request::get("/").body(Body::empty()).unwrap())
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::OK);
  assert!(response
    .headers()
    .contains_key(header::CONTENT_SECURITY_POLICY));
}

#[test]
fn files_and_catalog_survive_reopening() {
  let root = tempfile::tempdir().unwrap();
  let bytes = package("1.0.0", "sample");
  Store::open(root.path())
    .unwrap()
    .publish(bytes.clone(), metadata(), None)
    .unwrap();
  let store = Store::open(root.path()).unwrap();
  let (mut file, release) = store.download("sample", "1.0.0").unwrap();
  let mut stored = Vec::new();
  file.read_to_end(&mut stored).unwrap();
  assert_eq!(bytes, stored);
  assert_eq!(apell_package::digest(&stored), release.sha256);
}

#[tokio::test]
async fn request_validation_enforces_auth_fields_and_sizes_without_publication() {
  let (root, app) = app();
  let bytes = package("1.0.0", "sample");
  let mut wrong_token = publish_request(&bytes);
  wrong_token.headers_mut().insert(
    header::AUTHORIZATION,
    "Bearer wrong-token-with-at-least-thirty-two-characters"
      .parse()
      .unwrap(),
  );
  assert_eq!(
    app.clone().oneshot(wrong_token).await.unwrap().status(),
    StatusCode::UNAUTHORIZED
  );
  let response = app
    .clone()
    .oneshot(publish_request(b"not a package"))
    .await
    .unwrap();
  assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
  let huge_icon = vec![0; 256 * 1024 + 1];
  let request = Request::post("/api/v1/publish")
    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
    .header(
      header::CONTENT_TYPE,
      "multipart/form-data; boundary=fixture-boundary",
    )
    .body(Body::from(multipart(&bytes, &metadata(), Some(&huge_icon))))
    .unwrap();
  assert_eq!(
    app.clone().oneshot(request).await.unwrap().status(),
    StatusCode::PAYLOAD_TOO_LARGE
  );
  let mut duplicate = multipart(&bytes, &metadata(), None);
  duplicate.truncate(duplicate.len() - b"--fixture-boundary--\r\n".len());
  duplicate.extend_from_slice(b"--fixture-boundary\r\nContent-Disposition: form-data; name=\"metadata\"\r\n\r\n{}\r\n--fixture-boundary--\r\n");
  let request = Request::post("/api/v1/publish")
    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
    .header(
      header::CONTENT_TYPE,
      "multipart/form-data; boundary=fixture-boundary",
    )
    .body(Body::from(duplicate))
    .unwrap();
  assert_eq!(
    app.oneshot(request).await.unwrap().status(),
    StatusCode::BAD_REQUEST
  );
  assert_eq!(
    std::fs::read_dir(root.path().join("data/packages"))
      .unwrap()
      .count(),
    0
  );
}

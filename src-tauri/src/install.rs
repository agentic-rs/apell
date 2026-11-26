use crate::{
  commands::blocking,
  host::{string, Host},
};
use apell_core::{
  bundles::{
    BundleStore, GitHubSource, InstalledApplet, PackageSource, StoreSwitchSnapshot,
    MAX_BUNDLE_BYTES,
  },
  github::{GitHubClient, GitHubTarget},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestorePackageRequest {
  applet_id: String,
  expected: StoreSwitchSnapshot,
  bundle_base64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdatePackageRequest {
  applet_id: String,
  expected: StoreSwitchSnapshot,
}

pub async fn release(repository: &str) -> Result<Value, String> {
  let release = GitHubClient::new()?.resolve(repository).await?;
  serde_json::to_value(release).map_err(|_| "Could not encode GitHub release.".into())
}

fn file_bytes(params: &Value) -> Result<Option<Vec<u8>>, String> {
  let Some(encoded) = params.get("bundle_base64") else {
    return Ok(None);
  };
  if [
    "repository",
    "release_tag",
    "asset_name",
    "release_id",
    "asset_id",
  ]
  .iter()
  .any(|key| params.get(key).is_some())
  {
    return Err("Choose a file or a GitHub repository.".into());
  }
  let encoded = encoded.as_str().ok_or("Invalid package bytes.")?;
  decode_file_bytes(encoded).map(Some)
}

fn decode_file_bytes(encoded: &str) -> Result<Vec<u8>, String> {
  if encoded.len() > MAX_BUNDLE_BYTES.div_ceil(3) * 4 {
    return Err("Package exceeds 24 MiB.".into());
  }
  let bytes = STANDARD
    .decode(encoded)
    .map_err(|_| "Invalid package encoding.")?;
  if bytes.len() > MAX_BUNDLE_BYTES {
    return Err("Package exceeds 24 MiB.".into());
  }
  Ok(bytes)
}

fn github_target(params: &Value) -> Result<GitHubTarget, String> {
  let repository = string(params, "repository")?.to_owned();
  let release_tag = match params.get("release_tag") {
    None | Some(Value::Null) => None,
    Some(Value::String(tag)) => Some(tag.clone()),
    Some(_) => return Err("Invalid GitHub release tag.".into()),
  };
  Ok(GitHubTarget {
    repository,
    release_tag,
  })
}

fn check_release_snapshot(params: &Value, release_id: u64, asset_id: u64) -> Result<(), String> {
  let requested = params
    .get("release_id")
    .and_then(Value::as_u64)
    .ok_or("Find the GitHub release before installing.")?;
  let requested_asset = params
    .get("asset_id")
    .and_then(Value::as_u64)
    .ok_or("Find the GitHub package before installing.")?;
  if requested != release_id || requested_asset != asset_id {
    return Err("The GitHub release changed. Find the release again before installing.".into());
  }
  Ok(())
}

pub async fn install(app: AppHandle, host: &Host, params: Value) -> Result<Value, String> {
  let gate = host.store_gate.lock().await;
  let installed = install_inner(&app, host, params, None).await?;
  drop(gate);
  Ok(published_install_result(&app, host, installed))
}

pub async fn restore_package(app: &AppHandle, host: &Host, params: Value) -> Result<Value, String> {
  let gate = host.store_gate.lock().await;
  let store = host.bundles.clone();
  let installed = blocking(move || {
    let request: RestorePackageRequest =
      serde_json::from_value(params).map_err(|_| "Invalid package restore request.")?;
    let bytes = decode_file_bytes(&request.bundle_base64)?;
    store.restore_package(&request.applet_id, &request.expected, &bytes)
  })
  .await?;
  drop(gate);
  Ok(published_install_result(app, host, installed))
}

fn reviewed_github_source(
  bundles: &BundleStore,
  request: &UpdatePackageRequest,
) -> Result<GitHubSource, String> {
  let installed = bundles.installed(&request.applet_id)?;
  if request.expected.removed
    || request.expected.sha256 != installed.sha256
    || request.expected.source != installed.source
  {
    return Err("The installed applet changed. Review its details again.".into());
  }
  match installed.source {
    Some(PackageSource::Github(source)) => Ok(source),
    _ => Err("This applet does not use a GitHub release.".into()),
  }
}

pub async fn update(app: AppHandle, host: &Host, params: Value) -> Result<Value, String> {
  let request: UpdatePackageRequest =
    serde_json::from_value(params).map_err(|_| "Review the installed applet before updating.")?;
  let gate = host.store_gate.lock().await;
  let store = host.bundles.clone();
  let applet_id = request.applet_id.clone();
  let source = blocking(move || reviewed_github_source(&store, &request)).await?;
  let installed = install_inner(
    &app,
    host,
    json!({"repository":source.repository,"release_tag":source.release_tag,"asset_name":source.asset_name}),
    Some((applet_id, source)),
  )
  .await?;
  drop(gate);
  Ok(published_install_result(&app, host, installed))
}

async fn install_inner(
  app: &AppHandle,
  host: &Host,
  params: Value,
  update_source: Option<(String, GitHubSource)>,
) -> Result<InstalledApplet, String> {
  let local = tauri::async_runtime::spawn_blocking({
    let params = params.clone();
    move || file_bytes(&params)
  })
  .await
  .map_err(|_| "Could not read the package.")??;
  let (bytes, source, expected) = if let Some(bytes) = local {
    (bytes, None, None)
  } else {
    let github = GitHubClient::new()?;
    let release = github.resolve_target(&github_target(&params)?).await?;
    if let Some((_, source)) = &update_source {
      if source.repository_id != release.repository_id {
        return Err("The GitHub repository identity changed. This update was rejected.".into());
      }
    }
    let asset = if let Some(name) = params.get("asset_name").and_then(Value::as_str) {
      release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or("The chosen package is missing from this release.")?
    } else if release.assets.len() == 1 {
      &release.assets[0]
    } else {
      return Err("Choose an .apel asset from this release.".into());
    };
    if update_source.is_none() {
      check_release_snapshot(&params, release.release_id, asset.asset_id)?;
    }
    let source = release.source(&asset.name)?;
    let expected = asset.sha256.clone();
    let bytes = github
      .download(&release, &asset.name, move |received_bytes, total_bytes| {
        let _ = app.emit_to(
          "main",
          "install-progress",
          json!({"received_bytes":received_bytes,"total_bytes":total_bytes}),
        );
      })
      .await?;
    (bytes, Some(source), expected)
  };
  let store = host.bundles.clone();
  tauri::async_runtime::spawn_blocking(move || {
    if let Some((id, _)) = update_source {
      store.update_from_source(
        &id,
        &bytes,
        expected.as_deref(),
        source.ok_or("GitHub source is missing.")?.into(),
      )
    } else {
      store.install_with_source(&bytes, expected.as_deref(), source.map(Into::into))
    }
  })
  .await
  .map_err(|_| "Package installation was interrupted.")?
}

pub(crate) fn publish_installation_change(app: &AppHandle, applet_id: &str) {
  let _ = app.emit_to(
    "main",
    "apell-applet-installation-changed",
    json!({ "applet_id": applet_id }),
  );
}

pub(crate) fn published_install_result(
  app: &AppHandle,
  host: &Host,
  installed: InstalledApplet,
) -> Value {
  publish_installation_change(app, &installed.manifest.applet_id);
  json!({
    "restart_required": host.needs_applet_restart(&installed),
    "applet": installed,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use apell_core::bundles::{pack_directory, StoreChannel, StoreSource, API_VERSION};
  use std::fs;

  #[test]
  fn local_packages_require_bounded_binary_encoding_and_one_source() {
    assert_eq!(
      file_bytes(&json!({"bundle_base64":STANDARD.encode(b"PK fixture")}))
        .unwrap()
        .unwrap(),
      b"PK fixture"
    );
    for input in [
      json!({"bundle_base64":null}),
      json!({"bundle_base64":"!"}),
      json!({"bundle_base64":"","repository":"owner/repo"}),
      json!({"bundle_base64":"","release_id":1}),
      json!({"bundle_base64":"","release_tag":"nightly"}),
    ] {
      assert!(file_bytes(&input).is_err());
    }
    assert!(file_bytes(&json!({"repository":"owner/repo"}))
      .unwrap()
      .is_none());
  }

  #[test]
  fn github_install_preserves_the_explicit_selector_and_rejects_malformed_tags() {
    let target =
      github_target(&json!({"repository":"example/project","release_tag":"nightly"})).unwrap();
    assert_eq!(target.repository, "example/project");
    assert_eq!(target.release_tag.as_deref(), Some("nightly"));
    assert!(github_target(&json!({"repository":"example/project"}))
      .unwrap()
      .release_tag
      .is_none());
    for tag in [json!(1), json!(true), json!([])] {
      assert!(github_target(&json!({"repository":"example/project","release_tag":tag})).is_err());
    }
  }

  #[test]
  fn github_install_requires_the_selected_release_snapshot() {
    for params in [
      json!({}),
      json!({"release_id":null}),
      json!({"release_id":"9"}),
      json!({"release_id":8}),
    ] {
      assert!(check_release_snapshot(&params, 9, 11).is_err());
    }
    for params in [
      json!({"release_id":9}),
      json!({"release_id":9,"asset_id":"11"}),
      json!({"release_id":9,"asset_id":10}),
    ] {
      assert!(check_release_snapshot(&params, 9, 11).is_err());
    }
    assert!(check_release_snapshot(&json!({"release_id":9,"asset_id":11}), 9, 11).is_ok());
  }

  #[test]
  fn github_update_command_requires_the_complete_reviewed_snapshot() {
    for malformed in [
      json!({"applet_id":"sample"}),
      json!({"applet_id":"sample","expected":{"sha256":"hash","removed":false}}),
      json!({"applet_id":"sample","expected":{"source":null,"sha256":"hash"}}),
      json!({"applet_id":"sample","expected":{"source":null,"sha256":"hash","removed":false,"extra":true}}),
      json!({"applet_id":"sample","expected":{"source":null,"sha256":"hash","removed":false},"extra":true}),
    ] {
      assert!(serde_json::from_value::<UpdatePackageRequest>(malformed).is_err());
    }
  }

  #[test]
  fn github_update_binds_the_reviewed_hash_and_full_installation_source() {
    let root = tempfile::tempdir().unwrap();
    let staging = tempfile::tempdir().unwrap();
    fs::write(
      staging.path().join("manifest.json"),
      json!({
        "schema_version":1,"api_version":API_VERSION,"applet_id":"sample",
        "name":"Sample","version":"1.0.0","permissions":[]
      })
      .to_string(),
    )
    .unwrap();
    fs::write(staging.path().join("index.html"), "Sample").unwrap();
    let bytes = pack_directory(staging.path()).unwrap();
    let source = GitHubSource {
      repository_id: 10,
      repository: "example/project".into(),
      asset_name: "sample.apel".into(),
      release_tag: Some("v1.0.0".into()),
      prerelease: false,
    };
    let bundles = BundleStore::new(root.path().join("bundles"));
    let installed = bundles
      .install_with_source(&bytes, None, Some(source.clone().into()))
      .unwrap();
    let exact = StoreSwitchSnapshot {
      sha256: installed.sha256.clone(),
      source: installed.source,
      removed: false,
    };
    let check = |bundles: &BundleStore, expected| {
      reviewed_github_source(
        bundles,
        &UpdatePackageRequest {
          applet_id: "sample".into(),
          expected,
        },
      )
    };
    assert_eq!(check(&bundles, exact.clone()).unwrap(), source);
    for expected in [
      StoreSwitchSnapshot {
        sha256: "f".repeat(64),
        ..exact.clone()
      },
      StoreSwitchSnapshot {
        source: Some(
          GitHubSource {
            repository_id: 11,
            repository: "example/other".into(),
            ..source.clone()
          }
          .into(),
        ),
        ..exact.clone()
      },
      StoreSwitchSnapshot {
        source: Some(
          GitHubSource {
            release_tag: None,
            ..source.clone()
          }
          .into(),
        ),
        ..exact.clone()
      },
      StoreSwitchSnapshot {
        removed: true,
        ..exact.clone()
      },
    ] {
      assert!(check(&bundles, expected).is_err());
    }
    let file_installations = BundleStore::new(root.path().join("files"));
    file_installations.install(&bytes, &exact.sha256).unwrap();
    assert!(check(&file_installations, exact.clone()).is_err());
    assert!(check(
      &file_installations,
      StoreSwitchSnapshot {
        source: None,
        ..exact.clone()
      },
    )
    .is_err());

    let switched = bundles
      .switch_to_store_release(
        &bytes,
        &exact.sha256,
        StoreSource {
          store_url: "https://store.example".into(),
          applet_id: "sample".into(),
          channel: StoreChannel::Stable,
        },
        &exact,
      )
      .unwrap();
    assert_eq!(switched.sha256, exact.sha256);
    assert!(check(&bundles, exact.clone()).is_err());
    assert!(check(
      &bundles,
      StoreSwitchSnapshot {
        source: switched.source,
        ..exact.clone()
      },
    )
    .is_err());
    bundles.remove("sample", || Ok(())).unwrap();
    assert!(check(&bundles, exact).is_err());
  }
}

use crate::host::{string, Host};
use apell_core::{
  bundles::{GitHubSource, InstalledApplet, MAX_BUNDLE_BYTES},
  github::GitHubClient,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

pub async fn release(repository: &str) -> Result<Value, String> {
  let release = GitHubClient::new()?.resolve(repository).await?;
  serde_json::to_value(release).map_err(|_| "Could not encode GitHub release.".into())
}

fn file_bytes(params: &Value) -> Result<Option<Vec<u8>>, String> {
  let Some(encoded) = params.get("bundle_base64") else {
    return Ok(None);
  };
  if ["repository", "asset_name", "release_id", "asset_id"]
    .iter()
    .any(|key| params.get(key).is_some())
  {
    return Err("Choose a file or a GitHub repository.".into());
  }
  let encoded = encoded.as_str().ok_or("Invalid package bytes.")?;
  if encoded.len() > MAX_BUNDLE_BYTES.div_ceil(3) * 4 {
    return Err("Package exceeds 24 MiB.".into());
  }
  let bytes = STANDARD
    .decode(encoded)
    .map_err(|_| "Invalid package encoding.")?;
  if bytes.len() > MAX_BUNDLE_BYTES {
    return Err("Package exceeds 24 MiB.".into());
  }
  Ok(Some(bytes))
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
  install_inner(app, host, params, None).await
}

pub async fn update(app: AppHandle, host: &Host, applet_id: &str) -> Result<Value, String> {
  let store = host.bundles.clone();
  let id = applet_id.to_owned();
  let installed = tauri::async_runtime::spawn_blocking(move || store.installed(&id))
    .await
    .map_err(|_| "Could not read the installed applet.")??;
  let source = installed
    .source
    .ok_or("This applet was installed from a file. Choose a new .apel file to update it.")?;
  install_inner(
    app,
    host,
    json!({"repository":source.repository,"asset_name":source.asset_name}),
    Some((applet_id.to_owned(), source)),
  )
  .await
}

async fn install_inner(
  app: AppHandle,
  host: &Host,
  params: Value,
  update_source: Option<(String, GitHubSource)>,
) -> Result<Value, String> {
  let local = tauri::async_runtime::spawn_blocking({
    let params = params.clone();
    move || file_bytes(&params)
  })
  .await
  .map_err(|_| "Could not read the package.")??;
  let (bytes, source, expected) = if let Some(bytes) = local {
    (bytes, None, None)
  } else {
    let repository = string(&params, "repository")?;
    let github = GitHubClient::new()?;
    let release = github.resolve(repository).await?;
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
  let installed: InstalledApplet = tauri::async_runtime::spawn_blocking(move || {
    if let Some((id, _)) = update_source {
      store.update_from_source(
        &id,
        &bytes,
        expected.as_deref(),
        source.ok_or("GitHub source is missing.")?,
      )
    } else {
      store.install_with_source(&bytes, expected.as_deref(), source)
    }
  })
  .await
  .map_err(|_| "Package installation was interrupted.")??;
  serde_json::to_value(installed).map_err(|_| "Could not encode installed applet.".into())
}

#[cfg(test)]
mod tests {
  use super::*;
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
    ] {
      assert!(file_bytes(&input).is_err());
    }
    assert!(file_bytes(&json!({"repository":"owner/repo"}))
      .unwrap()
      .is_none());
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
}

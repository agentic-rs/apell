//! Debug-only startup handoff for `apell dev --install`. The CLI stages one
//! batch in the app container's cache before launching us.

use apell_core::bundles::{atomic_write, BundleStore, MAX_BUNDLE_BYTES};
use serde::Serialize;
use std::{
  fs::{self, File},
  io::Read,
  path::Path,
};

const ARG: &str = "--apell-dev-install=";
const DIRECTORY: &str = "apell-dev";
const RECEIPT: &str = "result.json";
const MAX_PACKAGES: usize = 16;

#[cfg(all(target_os = "ios", debug_assertions))]
pub(crate) fn start(app: &tauri::App, host: &crate::host::Host) {
  use tauri::Manager;

  let batch = match batch_id(std::env::args_os().filter_map(|arg| arg.into_string().ok())) {
    Ok(Some(batch)) => batch,
    Ok(None) => return,
    Err(error) => {
      eprintln!("Apell dev install: {error}");
      return;
    }
  };
  let Ok(cache) = app.path().cache_dir() else {
    eprintln!("Apell dev install: App cache is unavailable.");
    return;
  };
  // Acquire before the WebView starts so the first applet list waits for the
  // batch. Package IO and validation must stay off the iOS startup thread.
  let Ok(gate) = host.store_gate.clone().try_lock_owned() else {
    eprintln!("Apell dev install: Applet storage is busy.");
    return;
  };
  let store = host.bundles.clone();
  tauri::async_runtime::spawn_blocking(move || {
    let _gate = gate;
    if let Err(error) = process_batch(&cache, &batch, &store) {
      eprintln!("Apell dev install: {error}");
    }
  });
}

fn batch_id(args: impl IntoIterator<Item = String>) -> Result<Option<String>, &'static str> {
  let mut batches = args
    .into_iter()
    .filter_map(|arg| arg.strip_prefix(ARG).map(str::to_owned));
  let Some(batch) = batches.next() else {
    return Ok(None);
  };
  if batch.len() != 32
    || !batch
      .bytes()
      .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    || batches.next().is_some()
  {
    return Err("Invalid developer install batch ID.");
  }
  Ok(Some(batch))
}

#[derive(Serialize)]
#[serde(untagged)]
enum InstallResult {
  Installed { applet_id: String, version: String },
  Failed { error: String },
}

fn process_batch(cache: &Path, batch: &str, store: &BundleStore) -> Result<(), String> {
  let staging = cache.join(DIRECTORY);
  let directory = staging.join(batch);
  for path in [&staging, &directory] {
    if !fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
      return Err("Developer install batch directory is missing or invalid.".into());
    }
  }
  let receipt = directory.join(RECEIPT);
  if fs::symlink_metadata(&receipt).is_ok_and(|metadata| metadata.is_file()) {
    // Do not rerun a completed batch if Xcode relaunches with the same args.
    return Ok(());
  }
  let results = match package_count(&directory) {
    Ok(count) => (0..count)
      .map(|index| {
        let path = directory.join(format!("{index}.apel"));
        let result = install(&path, store);
        let _ = fs::remove_file(path);
        match result {
          Ok(result) => result,
          Err(error) => InstallResult::Failed { error },
        }
      })
      .collect(),
    Err(error) => vec![InstallResult::Failed { error }],
  };
  let bytes = serde_json::to_vec(&results).map_err(|_| "Could not encode install results.")?;
  atomic_write(&receipt, &bytes)
}

fn package_count(directory: &Path) -> Result<usize, String> {
  let mut seen = [false; MAX_PACKAGES];
  let mut count = 0;
  for entry in fs::read_dir(directory).map_err(|_| "Could not list staged applets.")? {
    let entry = entry.map_err(|_| "Could not inspect a staged applet.")?;
    let filename = entry.file_name();
    let index = filename
      .to_str()
      .and_then(|name| name.strip_suffix(".apel"))
      .and_then(|stem| {
        let index = stem.parse::<usize>().ok()?;
        (index < MAX_PACKAGES && stem == index.to_string()).then_some(index)
      })
      .ok_or("Unexpected file in developer install batch.")?;
    if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
      return Err("Staged applets must be regular files.".into());
    }
    seen[index] = true;
    count += 1;
  }
  if count == 0 || seen.iter().take(count).any(|present| !present) {
    return Err("Developer install batch must contain consecutive applet files.".into());
  }
  Ok(count)
}

fn install(path: &Path, store: &BundleStore) -> Result<InstallResult, String> {
  let file = File::open(path).map_err(|_| "Could not open staged applet.")?;
  let mut bytes = Vec::new();
  file
    .take(MAX_BUNDLE_BYTES as u64 + 1)
    .read_to_end(&mut bytes)
    .map_err(|_| "Could not read staged applet.")?;
  let installed = store.install_with_source(&bytes, None, None)?;
  Ok(InstallResult::Installed {
    applet_id: installed.manifest.applet_id,
    version: installed.manifest.version,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::{json, Value};

  const BATCH: &str = "0123456789abcdef0123456789abcdef";

  #[test]
  fn only_accepts_one_batch_id_without_paths() {
    assert_eq!(batch_id(["Apell".into()]), Ok(None));
    let valid = format!("{ARG}{BATCH}");
    assert_eq!(batch_id([valid.clone()]), Ok(Some(BATCH.into())));
    assert!(batch_id([valid.clone(), valid]).is_err());
    assert!(batch_id([format!("{ARG}../secrets")]).is_err());
  }

  #[test]
  fn installs_a_batch_in_index_order_and_reports_individual_errors() {
    let root = tempfile::tempdir().unwrap();
    let queue = root.path().join(DIRECTORY).join(BATCH);
    fs::create_dir_all(&queue).unwrap();
    let source = root.path().join("fixture");
    fs::create_dir(&source).unwrap();
    fs::write(
      source.join("manifest.json"),
      json!({
        "schema_version":1, "api_version":apell_core::bundles::API_VERSION,
        "applet_id":"sample", "version":"1.2.3", "name":"Sample", "permissions":[]
      })
      .to_string(),
    )
    .unwrap();
    fs::write(source.join("index.html"), "<p>Sample</p>").unwrap();
    fs::write(
      queue.join("1.apel"),
      apell_core::bundles::pack_directory(&source).unwrap(),
    )
    .unwrap();
    fs::write(queue.join("0.apel"), b"not a package").unwrap();

    let store = BundleStore::new(root.path().join("bundles"));
    process_batch(root.path(), BATCH, &store).unwrap();
    let receipt = fs::read(queue.join(RECEIPT)).unwrap();
    let results: Value = serde_json::from_slice(&receipt).unwrap();
    assert_eq!(results.as_array().unwrap().len(), 2);
    assert!(results[0]["error"].is_string());
    assert_eq!(results[1], json!({"applet_id":"sample","version":"1.2.3"}));
    assert_eq!(store.installed("sample").unwrap().manifest.version, "1.2.3");
    assert_eq!(fs::read_dir(&queue).unwrap().count(), 1);
    process_batch(root.path(), BATCH, &store).unwrap();
    assert_eq!(fs::read(queue.join(RECEIPT)).unwrap(), receipt);
  }

  #[test]
  fn rejects_gaps_and_symbolic_links_in_staged_packages() {
    let queue = tempfile::tempdir().unwrap();
    fs::write(queue.path().join("1.apel"), b"package").unwrap();
    assert!(package_count(queue.path()).is_err());
    #[cfg(unix)]
    {
      std::os::unix::fs::symlink(queue.path().join("1.apel"), queue.path().join("0.apel")).unwrap();
      assert!(package_count(queue.path()).is_err());
    }
  }
}

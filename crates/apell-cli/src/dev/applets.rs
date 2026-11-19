use apell_package::{parse, MAX_BUNDLE_BYTES};
use serde::Deserialize;
use std::{
  collections::HashSet,
  fs,
  io::Read,
  path::{Path, PathBuf},
  process::Command,
  thread,
  time::{Duration, Instant},
};
use tempfile::TempDir;

const STAGING_DIR: &str = "Library/Caches/apell-dev";
const RECEIPT: &str = "result.json";
const RECEIPT_LIMIT: u64 = 32 * 1024;
const INSTALL_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) struct Batch {
  _temp: TempDir,
  batch_id: String,
  staging: PathBuf,
  // A successful receipt must describe the exact packages we prepared.
  packages: Vec<(String, String)>,
}

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum InstallResult {
  Installed { applet_id: String, version: String },
  Failed { error: String },
}

impl Batch {
  pub(super) fn prepare(paths: &[PathBuf]) -> Result<Option<Self>, String> {
    if paths.is_empty() {
      return Ok(None);
    }
    let temp = tempfile::tempdir().map_err(|_| "Could not create temporary package storage.")?;
    let batch_id = uuid::Uuid::new_v4().simple().to_string();
    let staging = temp.path().join(&batch_id);
    fs::create_dir(&staging).map_err(|_| "Could not stage applet packages.")?;
    let mut packages = Vec::with_capacity(paths.len());
    let mut ids = HashSet::new();
    for (index, path) in paths.iter().enumerate() {
      let label = format!("Applet package {} ({})", index + 1, path.display());
      if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("apel"))
      {
        return Err(format!("{label} must have an .apel extension."));
      }
      // Check before opening, so a mistyped path to a pipe cannot block the CLI.
      if !fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return Err(format!("{label} must be a regular file."));
      }
      let bytes = crate::publish::input::read_bounded(path, MAX_BUNDLE_BYTES, &label)?;
      let manifest = parse(&bytes)
        .map_err(|error| format!("{label}: {error}"))?
        .manifest;
      if !ids.insert(manifest.applet_id.clone()) {
        return Err(format!(
          "More than one package has applet ID {}.",
          manifest.applet_id
        ));
      }
      fs::write(staging.join(format!("{index}.apel")), &bytes)
        .map_err(|_| "Could not stage an applet package.")?;
      packages.push((manifest.applet_id, manifest.version));
    }
    Ok(Some(Self {
      _temp: temp,
      batch_id,
      staging,
      packages,
    }))
  }

  pub(super) fn launch_argument(&self) -> String {
    format!("--apell-dev-install={}", self.batch_id)
  }

  pub(super) fn stage_simulator(&self, udid: &str, id: &str) -> Result<SimBatch, String> {
    let output = Command::new("xcrun")
      .args(["simctl", "get_app_container", udid, id, "data"])
      .output()
      .map_err(|_| "Could not locate Apell's simulator container.")?;
    let container =
      String::from_utf8(output.stdout).map_err(|_| "Invalid simulator container path.")?;
    let container = Path::new(container.trim_end());
    if !output.status.success() || !container.is_absolute() || !container.is_dir() {
      return Err("Could not locate Apell's simulator container.".into());
    }
    let parent = container.join(STAGING_DIR);
    fs::create_dir_all(&parent).map_err(|_| "Could not create the simulator install queue.")?;
    let directory = parent.join(&self.batch_id);
    fs::create_dir(&directory).map_err(|_| "Could not create the simulator install batch.")?;
    let batch = SimBatch(directory);
    for index in 0..self.packages.len() {
      let name = format!("{index}.apel");
      fs::copy(self.staging.join(&name), batch.0.join(&name))
        .map_err(|_| "Could not copy an applet to the simulator.")?;
    }
    Ok(batch)
  }

  fn remote_directory(&self) -> String {
    format!("{STAGING_DIR}/{}", self.batch_id)
  }

  pub(super) fn stage_device(&self, udid: &str, id: &str) -> Result<(), String> {
    let status = device_copy("to", udid, id, "45")
      .arg("--source")
      .arg(&self.staging)
      .arg("--destination")
      .arg(self.remote_directory())
      .status()
      .map_err(|e| format!("Could not copy applets to the iPhone: {e}"))?;
    if !status.success() {
      return Err(
        "Could not copy applets to the iPhone. Unlock it and check the connection.".into(),
      );
    }
    Ok(())
  }

  pub(super) fn wait_simulator(&self, batch: &SimBatch) -> Result<(), String> {
    let receipt = batch.0.join(RECEIPT);
    self.wait_for_receipt(&receipt, || receipt.is_file(), Duration::from_millis(200))
  }

  pub(super) fn wait_device(&self, udid: &str, id: &str) -> Result<(), String> {
    let receipt = self._temp.path().join(RECEIPT);
    let remote = format!("{}/{RECEIPT}", self.remote_directory());
    self.wait_for_receipt(
      &receipt,
      || {
        device_copy("from", udid, id, "5")
          .args(["--source", &remote, "--destination"])
          .arg(&receipt)
          .output()
          .is_ok_and(|output| output.status.success() && receipt.is_file())
      },
      Duration::from_secs(1),
    )
  }

  fn wait_for_receipt(
    &self,
    receipt: &Path,
    ready: impl Fn() -> bool,
    poll: Duration,
  ) -> Result<(), String> {
    let deadline = Instant::now() + INSTALL_TIMEOUT;
    while Instant::now() < deadline {
      if ready() {
        return self.report(receipt);
      }
      thread::sleep(poll);
    }
    Err("Apell launched, but did not report the applet install results. Check the app library before retrying.".into())
  }

  fn report(&self, receipt: &Path) -> Result<(), String> {
    let mut bytes = Vec::new();
    fs::File::open(receipt)
      .and_then(|file| file.take(RECEIPT_LIMIT + 1).read_to_end(&mut bytes))
      .map_err(|_| "Could not read applet install results.")?;
    if bytes.len() as u64 > RECEIPT_LIMIT {
      return Err("Invalid applet install results.".into());
    }
    let results: Vec<InstallResult> =
      serde_json::from_slice(&bytes).map_err(|_| "Invalid applet install results.")?;
    if results.len() != self.packages.len() {
      if let [InstallResult::Failed { error }] = results.as_slice() {
        return Err(format!("Could not install the applet batch: {error}"));
      }
      return Err(
        "Incomplete applet install results. Check the app library before retrying.".into(),
      );
    }
    let mut failures = 0;
    for ((expected_id, expected_version), result) in self.packages.iter().zip(results) {
      match result {
        InstallResult::Installed { applet_id, version }
          if (&applet_id, &version) == (expected_id, expected_version) =>
        {
          println!("Installed {applet_id} {version}");
        }
        InstallResult::Installed { .. } => {
          return Err("Applet install results did not match the requested packages.".into());
        }
        InstallResult::Failed { error } => {
          eprintln!("Could not install {expected_id}: {error}");
          failures += 1;
        }
      }
    }
    if failures != 0 {
      return Err(format!(
        "Could not install {failures} of {} applets.",
        self.packages.len()
      ));
    }
    Ok(())
  }
}

pub(super) struct SimBatch(PathBuf);

impl Drop for SimBatch {
  fn drop(&mut self) {
    let _ = fs::remove_dir_all(&self.0);
  }
}

fn device_copy(direction: &str, udid: &str, id: &str, timeout: &str) -> Command {
  let mut command = Command::new("xcrun");
  command.args([
    "devicectl",
    "device",
    "copy",
    direction,
    "--device",
    udid,
    "--domain-type",
    "appDataContainer",
    "--domain-identifier",
    id,
    "--quiet",
    "--timeout",
    timeout,
  ]);
  command
}

use super::{
  applets::Batch,
  targets::{Kind, Target},
};
use serde::Deserialize;
use std::{
  env,
  ffi::OsStr,
  fs,
  os::unix::fs::PermissionsExt,
  path::{Path, PathBuf},
  process::Command,
};
use tempfile::TempDir;

pub(super) fn run(project: &Path, target: &Target, batch: Option<&Batch>) -> Result<(), String> {
  let apple = project.join("src-tauri/gen/apple");
  if !apple.exists() {
    run_command(
      Command::new("pnpm")
        .current_dir(project)
        .args(["tauri", "ios", "init", "--ci"]),
      "initialize the iOS project",
    )?;
  }
  let mut build = Command::new("pnpm");
  build
    .current_dir(project)
    .args(["tauri", "ios", "build", "--debug", "--ci"]);
  // Tauri synchronizes signing settings into the generated project on each build.
  // Keep the bundle identifier and team selected in Xcode for this SDK.
  let signing = local_signing(&apple, target.kind)?;
  let mut config = serde_json::json!({"identifier":signing.product_bundle_identifier});
  if let Ok(team) = env::var("APPLE_DEVELOPMENT_TEAM") {
    if !team.trim().is_empty() {
      config["bundle"]["iOS"]["developmentTeam"] = team.into();
    }
  } else if let Some(team) = signing.development_team.filter(|team| !team.is_empty()) {
    build.env("APPLE_DEVELOPMENT_TEAM", &team);
    config["bundle"]["iOS"]["developmentTeam"] = team.into();
  }
  build.arg("--config").arg(config.to_string());
  match target.kind {
    Kind::Simulator => run_simulator(build, &apple, &target.udid, batch),
    Kind::Device => run_device(build, &target.udid, batch),
  }
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
struct Signing {
  product_bundle_identifier: String,
  development_team: Option<String>,
}

fn local_signing(apple: &Path, kind: Kind) -> Result<Signing, String> {
  #[derive(Deserialize)]
  struct Settings {
    target: String,
    #[serde(rename = "buildSettings")]
    settings: serde_json::Value,
  }
  let destination = match kind {
    Kind::Simulator => "generic/platform=iOS Simulator",
    Kind::Device => "generic/platform=iOS",
  };
  let output = Command::new("xcrun")
    .current_dir(apple)
    .args([
      "xcodebuild",
      "-project",
      "apell.xcodeproj",
      "-scheme",
      "apell_iOS",
      "-configuration",
      "debug",
      "-destination",
      destination,
      "-showBuildSettings",
      "-json",
    ])
    .output()
    .map_err(|e| format!("Could not read Xcode settings: {e}"))?;
  if !output.status.success() {
    return Err(format!(
      "Could not read Xcode build settings. {}",
      String::from_utf8_lossy(&output.stderr).trim()
    ));
  }
  let settings: Vec<Settings> =
    serde_json::from_slice(&output.stdout).map_err(|_| "Invalid Xcode build settings response.")?;
  let target = settings
    .into_iter()
    .find(|s| s.target == "apell_iOS")
    .ok_or("The generated project is missing the apell_iOS target.")?;
  let signing: Signing = serde_json::from_value(target.settings)
    .map_err(|_| "Set the bundle identifier in the apell_iOS target's signing settings.")?;
  if signing.product_bundle_identifier.is_empty() {
    return Err("Set the bundle identifier in the apell_iOS target's signing settings.".into());
  }
  Ok(signing)
}

fn run_command(command: &mut Command, action: &str) -> Result<(), String> {
  let status = command
    .status()
    .map_err(|e| format!("Could not {action}: {e}"))?;
  if status.success() {
    Ok(())
  } else {
    Err(format!("Failed to {action} ({status})."))
  }
}

fn app_in(directory: &Path) -> Result<PathBuf, String> {
  let entries =
    fs::read_dir(directory).map_err(|e| format!("Could not locate the built application: {e}"))?;
  let mut apps = vec![];
  for entry in entries {
    let path = entry.map_err(|e| e.to_string())?.path();
    if path.extension() == Some(OsStr::new("app")) && path.join("Info.plist").is_file() {
      apps.push(path);
    }
  }
  match apps.as_slice() {
    [app] => Ok(app.clone()),
    _ => Err("Expected one built .app in the iOS build output.".into()),
  }
}

fn bundle_identifier(app: &Path) -> Result<String, String> {
  let output = Command::new("/usr/bin/plutil")
    .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-", "--"])
    .arg(app.join("Info.plist"))
    .output()
    .map_err(|e| e.to_string())?;
  let id =
    String::from_utf8(output.stdout).map_err(|_| "Invalid application bundle identifier.")?;
  let id = id.trim();
  if !output.status.success()
    || id.is_empty()
    || !id
      .bytes()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-'))
  {
    return Err("Unable to read the built application's bundle identifier.".into());
  }
  Ok(id.into())
}

fn run_simulator(
  mut build: Command,
  apple: &Path,
  udid: &str,
  batch: Option<&Batch>,
) -> Result<(), String> {
  let (arch, output) = if cfg!(target_arch = "aarch64") {
    ("aarch64-sim", "arm64-sim")
  } else {
    ("x86_64", "x86_64")
  };
  run_command(
    build.args(["--target", arch]),
    "build the signed simulator app",
  )?;
  let app = app_in(&apple.join("build").join(output))?;
  let id = bundle_identifier(&app)?;
  run_command(
    Command::new("xcrun").args(["simctl", "bootstatus", udid, "-b"]),
    "boot the selected simulator",
  )?;
  run_command(
    Command::new("xcrun")
      .args(["simctl", "install", udid])
      .arg(&app),
    "install Apell on the simulator",
  )?;
  run_command(
    Command::new("open").args(["-a", "Simulator", "--args", "-CurrentDeviceUDID", udid]),
    "open Simulator",
  )?;
  let staged = batch
    .map(|batch| batch.stage_simulator(udid, &id))
    .transpose()?;
  let mut launch = Command::new("xcrun");
  launch.args(["simctl", "launch", "--terminate-running-process", udid, &id]);
  if let Some(batch) = batch {
    launch.arg(batch.launch_argument());
  }
  run_command(&mut launch, "launch Apell on the simulator")?;
  if let Some((batch, staged)) = batch.zip(staged.as_ref()) {
    batch.wait_simulator(staged)?;
  }
  Ok(())
}

// Tauri hosts its Rust build-options service for the entire archive. Its iOS
// build command exposes the architecture but no archive destination or device
// registration switch. Adapt that one Xcode invocation and give this run its
// own archive, so a previous generic archive can never be installed by mistake.
const DEVICE_ARCHIVER: &str = r#"#!/bin/bash
set -eu
settings=${0%/*}
IFS= read -r tool < "$settings/tool"
IFS= read -r udid < "$settings/udid"
for option in "$@"; do
  if [[ $option == archive ]]; then
    args=()
    while (( $# )); do
      case "$1" in
        -archivePath|-destination) shift 2 ;;
        *) args+=("$1"); shift ;;
      esac
    done
    exec "$tool" "${args[@]}" -archivePath "$settings/Apell.xcarchive" \
      -destination "id=$udid" -allowProvisioningDeviceRegistration CODE_SIGN_STYLE=Automatic
  fi
done
exec "$tool" "$@"
"#;

fn prepare_archiver(directory: &Path, udid: &str, build: &mut Command) -> Result<(), String> {
  let tool = Command::new("xcrun")
    .args(["--find", "xcodebuild"])
    .output()
    .map_err(|e| e.to_string())?;
  let tool_path = String::from_utf8(tool.stdout).map_err(|_| "Invalid Xcode tool path.")?;
  let tool_path = tool_path.trim_end();
  if !tool.status.success()
    || !Path::new(tool_path).is_absolute()
    || tool_path.contains(['\r', '\n'])
  {
    return Err("Could not locate xcodebuild. Select Xcode with xcode-select.".into());
  }
  let shim = directory.join("xcodebuild");
  fs::write(&shim, DEVICE_ARCHIVER)
    .and_then(|_| fs::write(directory.join("tool"), format!("{tool_path}\n")))
    .and_then(|_| fs::write(directory.join("udid"), format!("{udid}\n")))
    .and_then(|_| fs::set_permissions(&shim, fs::Permissions::from_mode(0o700)))
    .map_err(|e| format!("Could not prepare Xcode provisioning: {e}"))?;
  let original = env::var_os("PATH").ok_or("Missing PATH.")?;
  let paths = std::iter::once(directory.to_owned()).chain(env::split_paths(&original));
  build.env("PATH", env::join_paths(paths).map_err(|e| e.to_string())?);
  Ok(())
}

fn run_device(mut build: Command, udid: &str, batch: Option<&Batch>) -> Result<(), String> {
  let temp =
    TempDir::new().map_err(|e| format!("Could not create a temporary archive directory: {e}"))?;
  prepare_archiver(temp.path(), udid, &mut build)?;
  run_command(
    build.args(["--target", "aarch64", "--archive-only"]),
    "provision and build Apell (check the development team and Apple Account in Xcode)",
  )?;
  let app = app_in(&temp.path().join("Apell.xcarchive/Products/Applications"))?;
  if !app.join("embedded.mobileprovision").is_file() {
    return Err("The device build has no embedded provisioning profile. Check Signing & Capabilities in Xcode.".into());
  }
  let id = bundle_identifier(&app)?;
  println!("Installing the provisioned build on the selected iPhone…");
  run_command(
    Command::new("xcrun")
      .args(["devicectl", "device", "install", "app", "--device", udid])
      .arg(&app),
    "install Apell on the selected iPhone",
  )
  .map_err(|error| format!("{error} Unlock the iPhone and check its connection in Xcode. If Wi-Fi times out, connect by USB and rerun the command."))?;
  if let Some(batch) = batch {
    batch.stage_device(udid, &id)?;
  }
  let mut launch = Command::new("xcrun");
  launch.args([
    "devicectl",
    "device",
    "process",
    "launch",
    "--device",
    udid,
    "--terminate-existing",
    &id,
  ]);
  if let Some(batch) = batch {
    launch.arg("--").arg(batch.launch_argument());
  }
  run_command(
    &mut launch,
    "launch Apell (unlock the iPhone and enable Developer Mode if prompted)",
  )?;
  if let Some(batch) = batch {
    batch.wait_device(udid, &id)?;
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn archiver_preserves_xcode_arguments_and_provisions_only_the_selected_device() {
    let temp = tempfile::Builder::new()
      .prefix("xcode adapter ")
      .tempdir()
      .unwrap();
    let root = temp.path();
    let shim = root.join("xcodebuild");
    fs::write(&shim, DEVICE_ARCHIVER).unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o700)).unwrap();
    // The stand-in Xcode executable echoes each argument on a separate line.
    let executable = root.join("argument printer");
    fs::write(&executable, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("tool"), format!("{}\n", executable.display())).unwrap();
    fs::write(root.join("udid"), "test-device\n").unwrap();
    let arguments = |args: &[&str]| {
      let result = Command::new(&shim).args(args).output().unwrap();
      assert!(result.status.success());
      String::from_utf8(result.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>()
    };
    assert_eq!(
      arguments(&["-version", "-sdk", "iphoneos"]),
      ["-version", "-sdk", "iphoneos"]
    );
    let args = arguments(&[
      "-scheme",
      "A B",
      "-allowProvisioningUpdates",
      "-destination",
      "generic/platform=iOS",
      "archive",
      "-archivePath",
      "old archive",
    ]);
    assert_eq!(
      args,
      [
        "-scheme",
        "A B",
        "-allowProvisioningUpdates",
        "archive",
        "-archivePath",
        root.join("Apell.xcarchive").to_str().unwrap(),
        "-destination",
        "id=test-device",
        "-allowProvisioningDeviceRegistration",
        "CODE_SIGN_STYLE=Automatic"
      ]
    );
  }
}

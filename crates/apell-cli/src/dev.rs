#[cfg(target_os = "macos")]
mod runner;
#[cfg(target_os = "macos")]
mod targets;

#[cfg(target_os = "macos")]
use std::path::Path;
use std::{ffi::OsString, path::PathBuf};

pub const USAGE: &str =
  "Usage: apell dev [--list] [--target simulator|device|NAME|UDID] [--project PATH]
Build and launch the Apell shell from a checkout. Run without --target to choose an iPhone.";

#[derive(Default)]
struct Options {
  list: bool,
  target: Option<String>,
  project: Option<PathBuf>,
}

impl Options {
  fn parse(args: &[OsString]) -> Result<Self, String> {
    let mut options = Self::default();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
      match arg.to_str() {
        Some("--list") if !options.list => options.list = true,
        Some("--target") if options.target.is_none() => {
          let value = args
            .next()
            .and_then(|arg| arg.to_str())
            .filter(|value| !value.is_empty() && !value.starts_with("--"))
            .ok_or(USAGE)?;
          options.target = Some(value.to_owned());
        }
        Some("--project") if options.project.is_none() => {
          let value = args
            .next()
            .filter(|arg| !arg.is_empty() && !arg.to_string_lossy().starts_with("--"))
            .ok_or(USAGE)?;
          options.project = Some(value.into());
        }
        _ => return Err(USAGE.into()),
      }
    }
    if options.list
      && options
        .target
        .as_deref()
        .is_some_and(|target| !matches!(target, "simulator" | "device"))
    {
      return Err("--list accepts --target simulator or --target device.".into());
    }
    Ok(options)
  }
}

#[cfg(target_os = "macos")]
fn checkout(path: &Path) -> Result<PathBuf, String> {
  let start = path
    .canonicalize()
    .map_err(|error| format!("Could not open the project directory: {error}"))?;
  if !start.is_dir() {
    return Err("--project must name a directory.".into());
  }
  start
    .ancestors()
    .find(|root| {
      root.join("scripts/prepare-ios.ts").is_file()
        && root.join("src-tauri/tauri.ios.conf.json").is_file()
        && root.join("crates/apell-cli/Cargo.toml").is_file()
    })
    .map(Path::to_path_buf)
    .ok_or_else(|| "Run this command from the Apell shell checkout, or pass --project PATH.".into())
}

pub fn run(args: &[OsString]) -> Result<(), String> {
  if matches!(args, [arg] if arg == "--help" || arg == "-h") {
    println!("{USAGE}");
    return Ok(());
  }
  let options = Options::parse(args)?;
  #[cfg(target_os = "macos")]
  {
    use targets::Kind;
    let filter = match options.target.as_deref() {
      Some("simulator") => Some(Kind::Simulator),
      Some("device") => Some(Kind::Device),
      _ => None,
    };
    if options.list {
      targets::list(&targets::discover(filter)?);
      return Ok(());
    }
    let path = options.project.unwrap_or_else(|| PathBuf::from("."));
    let root = checkout(&path)?;
    let available = targets::discover(filter)?;
    let target = targets::select(&available, options.target.as_deref())?;
    // Both SDKs use the same generated Xcode project and archive paths.
    let checkout_lock = std::fs::File::open(root.join("src-tauri"))
      .map_err(|error| format!("Could not open the checkout: {error}"))?;
    checkout_lock.try_lock().map_err(|error| match error {
      std::fs::TryLockError::WouldBlock => {
        "Another apell dev run is using this checkout.".to_owned()
      }
      std::fs::TryLockError::Error(error) => format!("Could not lock the checkout: {error}"),
    })?;
    println!("Running Apell on {target}");
    runner::run(&root, &target)
  }
  #[cfg(not(target_os = "macos"))]
  {
    let _ = options;
    Err("Running on an iPhone requires a Mac with Xcode.".into())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn missing_or_ambiguous_arguments_do_not_start_target_discovery() {
    for args in [
      &["--list", "--list"][..],
      &["--target", "--project", "checkout"],
      &["--project"],
      &["--target", "device", "--target", "simulator"],
      &["--list", "--target", "some-device"],
    ] {
      let args = args.iter().map(OsString::from).collect::<Vec<_>>();
      assert!(Options::parse(&args).is_err());
    }
  }
}

use std::{
  fs::{DirBuilder, File, OpenOptions},
  os::unix::fs::DirBuilderExt,
  path::Path,
};
use tauri::{utils::config::AppDirectoriesOverride, Context, Wry};

pub(crate) fn device_context(
  mut context: Context<Wry>,
) -> Result<(Context<Wry>, Option<File>), String> {
  let Some(name) = std::env::var_os("APELL_LOCAL_DEVICE") else {
    return Ok((context, None));
  };
  let name = name
    .to_str()
    .filter(|name| valid_device_name(name))
    .ok_or("Use 1–32 lowercase letters, digits, or hyphens, starting with a letter or digit.")?;
  if context.config().identifier != tauri_plugin_apell_credentials::LOCAL_BUNDLE_IDENTIFIER {
    return Err("Local device profiles require Apell Local.".into());
  }
  let root = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../deploy/local/.state/devices")
    .join(name);
  DirBuilder::new()
    .recursive(true)
    .mode(0o700)
    .create(&root)
    .map_err(|_| "Could not create local device data.")?;
  let lease = OpenOptions::new()
    .create(true)
    .write(true)
    .truncate(false)
    .open(root.join("device.lock"))
    .map_err(|_| "Could not open the local device lock.")?;
  lease
    .try_lock()
    .map_err(|_| "This local device is already open. Use a different device name.")?;
  let config = context.config_mut();
  config.product_name = Some(format!("Apell Local ({name})"));
  config.app.app_directories_override = Some(AppDirectoriesOverride::Root(root));
  Ok((context, Some(lease)))
}

fn valid_device_name(name: &str) -> bool {
  let alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
  name.len() <= 32
    && name.bytes().next().is_some_and(alphanumeric)
    && name.bytes().all(|byte| alphanumeric(byte) || byte == b'-')
}

#[cfg(test)]
mod tests {
  use super::valid_device_name;

  #[test]
  fn local_device_names_are_single_portable_path_components() {
    assert!(valid_device_name("phone-2"));
    for invalid in ["", "../phone", "laptop/phone", "-phone", "Phone", "a b"] {
      assert!(!valid_device_name(invalid));
    }
    assert!(!valid_device_name(&"a".repeat(33)));
  }
}

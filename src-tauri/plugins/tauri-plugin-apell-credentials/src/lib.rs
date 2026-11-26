#[cfg(all(feature = "local-dev", not(all(target_os = "macos", debug_assertions))))]
compile_error!("The local-dev credential store requires a macOS debug build.");

#[cfg(any(
  target_os = "android",
  all(target_os = "macos", debug_assertions, feature = "local-dev")
))]
use tauri::Manager;
use tauri::{AppHandle, Runtime};

#[cfg(all(target_os = "macos", debug_assertions, feature = "local-dev"))]
mod local;

const MAX_CREDENTIAL_BYTES: usize = 128 * 1024;

#[cfg(target_os = "android")]
struct AndroidStore<R: Runtime>(tauri::plugin::PluginHandle<R>);

pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
  tauri::plugin::Builder::new("apell-credentials")
    .setup(|_app, _api| {
      #[cfg(all(target_os = "macos", debug_assertions, feature = "local-dev"))]
      {
        if _app.config().identifier != LOCAL_BUNDLE_IDENTIFIER {
          return Err(
            std::io::Error::other("The local credential store requires Apell Local.").into(),
          );
        }
        let root = _app.path().app_data_dir().map_err(|_| {
          std::io::Error::other("Could not locate the local test credential store.")
        })?;
        _app.manage(local::Credentials::new(root.join("mock-credentials")));
      }
      #[cfg(target_os = "android")]
      _app.manage(AndroidStore(_api.register_android_plugin(
        "dev.apell.credentials",
        "CredentialsPlugin",
      )?));
      Ok(())
    })
    .build()
}

#[cfg(target_os = "macos")]
pub const LOCAL_BUNDLE_IDENTIFIER: &str = "dev.apell.shell.local";

#[cfg(all(target_os = "macos", not(feature = "local-dev")))]
fn macos_service(identifier: &str) -> Result<&'static str, String> {
  // A misconfigured Apell Local must never open the system Keychain.
  if identifier == LOCAL_BUNDLE_IDENTIFIER {
    Err("Local test credentials require a local-dev build.".into())
  } else {
    Ok("dev.apell.llm")
  }
}

#[cfg(any(
  target_os = "ios",
  all(target_os = "macos", not(feature = "local-dev"))
))]
fn entry<R: Runtime>(_app: &AppHandle<R>, account: &str) -> Result<keyring_core::Entry, String> {
  #[cfg(target_os = "macos")]
  let result = apple_native_keyring_store::keychain::Cred::build(
    apple_native_keyring_store::keychain::MacKeychainDomain::User,
    macos_service(&_app.config().identifier)?,
    account,
  );
  #[cfg(target_os = "ios")]
  let result = apple_native_keyring_store::protected::Cred::build(
    "dev.apell.llm",
    account,
    apple_native_keyring_store::protected::AccessPolicy::WhenUnlockedThisDeviceOnly,
    None,
    false,
  );
  result.map_err(|_| "Could not access the system credential store.".into())
}

pub fn read<R: Runtime>(_app: &AppHandle<R>, account: &str) -> Result<Option<String>, String> {
  #[cfg(all(target_os = "macos", debug_assertions, feature = "local-dev"))]
  {
    _app.state::<local::Credentials>().read(account)
  }
  #[cfg(any(
    target_os = "ios",
    all(target_os = "macos", not(feature = "local-dev"))
  ))]
  {
    match entry(_app, account)?.get_password() {
      Ok(value) => Ok(Some(value)),
      Err(keyring_core::Error::NoEntry) => Ok(None),
      Err(_) => Err("Could not read the credential from Keychain.".into()),
    }
  }
  #[cfg(target_os = "android")]
  {
    #[derive(serde::Serialize)]
    struct Args<'a> {
      account: &'a str,
    }
    #[derive(serde::Deserialize)]
    struct Response {
      value: Option<String>,
    }
    _app
      .state::<AndroidStore<R>>()
      .0
      .run_mobile_plugin::<Response>("read", Args { account })
      .map(|r| r.value)
      .map_err(|_| "Could not read the Android credential store.".into())
  }
  #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
  {
    let _ = account;
    Err("Secure credentials are supported on iOS, Android, and macOS.".into())
  }
}

pub fn write<R: Runtime>(
  _app: &AppHandle<R>,
  account: &str,
  value: Option<&str>,
) -> Result<(), String> {
  if value.is_some_and(|value| value.len() > MAX_CREDENTIAL_BYTES) {
    return Err("The secure credential exceeds its size limit.".into());
  }
  #[cfg(all(target_os = "macos", debug_assertions, feature = "local-dev"))]
  {
    _app.state::<local::Credentials>().write(account, value)
  }
  #[cfg(any(
    target_os = "ios",
    all(target_os = "macos", not(feature = "local-dev"))
  ))]
  {
    let entry = entry(_app, account)?;
    if let Some(value) = value {
      entry
        .set_password(value)
        .map_err(|_| "Could not save the credential in Keychain.".into())
    } else {
      match entry.delete_credential() {
        Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
        Err(_) => Err("Could not remove the credential from Keychain.".into()),
      }
    }
  }
  #[cfg(target_os = "android")]
  {
    #[derive(serde::Serialize)]
    struct Args<'a> {
      account: &'a str,
      value: Option<&'a str>,
    }
    _app
      .state::<AndroidStore<R>>()
      .0
      .run_mobile_plugin::<serde::de::IgnoredAny>("write", Args { account, value })
      .map(|_| ())
      .map_err(|_| "Could not save the Android credential.".into())
  }
  #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "android")))]
  {
    let _ = (account, value);
    Err("Secure credentials are supported on iOS, Android, and macOS.".into())
  }
}

#[cfg(all(test, target_os = "macos", not(feature = "local-dev")))]
mod tests {
  use super::macos_service;

  #[test]
  fn only_the_reserved_local_identifier_is_blocked_from_keychain() {
    assert!(macos_service("dev.apell.shell.local").is_err());
    assert_eq!(macos_service("dev.apell.shell").unwrap(), "dev.apell.llm");
    assert_eq!(
      macos_service("dev.apell.shell.local.preview").unwrap(),
      "dev.apell.llm"
    );
  }
}

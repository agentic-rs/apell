#[cfg(target_os = "android")]
use tauri::Manager;
use tauri::{AppHandle, Runtime};

#[cfg(target_os = "android")]
struct AndroidStore<R: Runtime>(tauri::plugin::PluginHandle<R>);

pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
  tauri::plugin::Builder::new("apell-credentials")
    .setup(|_app, _api| {
      #[cfg(target_os = "android")]
      _app.manage(AndroidStore(_api.register_android_plugin(
        "dev.apell.credentials",
        "CredentialsPlugin",
      )?));
      Ok(())
    })
    .build()
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn entry(account: &str) -> Result<keyring_core::Entry, String> {
  #[cfg(target_os = "macos")]
  let result = apple_native_keyring_store::keychain::Cred::build(
    apple_native_keyring_store::keychain::MacKeychainDomain::User,
    "dev.apell.llm",
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
  #[cfg(any(target_os = "macos", target_os = "ios"))]
  {
    match entry(account)?.get_password() {
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
  #[cfg(any(target_os = "macos", target_os = "ios"))]
  {
    let entry = entry(account)?;
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

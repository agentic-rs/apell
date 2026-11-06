//! Internal native sensor operations. The shell verifies all applet requests.
use serde_json::Value;
#[cfg(any(target_os = "ios", target_os = "android"))]
use tauri::Manager;
use tauri::{AppHandle, Runtime};

#[cfg(any(target_os = "ios", target_os = "android"))]
struct Sensors<R: Runtime>(tauri::plugin::PluginHandle<R>);

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_apell_sensors);

pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
  tauri::plugin::Builder::new("apell-sensors")
    .setup(|_app, _api| {
      #[cfg(target_os = "ios")]
      _app.manage(Sensors(
        _api.register_ios_plugin(init_plugin_apell_sensors)?,
      ));
      #[cfg(target_os = "android")]
      _app.manage(Sensors(
        _api.register_android_plugin("dev.apell.sensors", "SensorsPlugin")?,
      ));
      Ok(())
    })
    .build()
}

pub fn available() -> bool {
  cfg!(any(target_os = "ios", target_os = "android"))
}

pub enum Command {
  Run,
  Cancel,
  ShowTransferQr,
  ScanTransferQr,
  SaveTransferFile,
}

/// Call from a blocking worker so native UI can wait for the user.
pub fn call<R: Runtime>(
  _app: &AppHandle<R>,
  _command: Command,
  _params: Value,
) -> Result<Value, String> {
  #[cfg(any(target_os = "ios", target_os = "android"))]
  {
    let method = match _command {
      Command::Run => "run",
      Command::Cancel => "cancel",
      Command::ShowTransferQr => "showTransferQr",
      Command::ScanTransferQr => "scanTransferQr",
      Command::SaveTransferFile => "saveTransferFile",
    };
    _app
      .state::<Sensors<R>>()
      .0
      .run_mobile_plugin::<Value>(method, _params)
      .map_err(|error| match error {
        tauri::plugin::mobile::PluginInvokeError::InvokeRejected(error) => error
          .message
          .unwrap_or_else(|| "The native request could not be completed.".into()),
        _ => "The native request could not be completed.".into(),
      })
  }
  #[cfg(not(any(target_os = "ios", target_os = "android")))]
  {
    Err("Camera, photo library, and location require iOS or Android.".into())
  }
}

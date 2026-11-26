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

/// Read system access without requesting a permission or starting a sensor.
/// Invoke from a blocking worker because the mobile adapter runs on the main thread.
pub fn system_authorization<R: Runtime>(
  app: &AppHandle<R>,
  permission: &str,
) -> Result<String, String> {
  if !matches!(permission, "camera" | "location") {
    return Err("Choose camera or location access.".into());
  }
  if !available() {
    return Ok("unknown".into());
  }
  let result = call(
    app,
    Command::SystemAuthorization,
    serde_json::json!({"permission": permission}),
  )?;
  result["status"]
    .as_str()
    .map(str::to_owned)
    .ok_or_else(|| "Could not check the system sensor permission.".into())
}

/// Open Apell's own system settings from a blocking shell worker.
pub fn open_system_settings<R: Runtime>(_app: &AppHandle<R>) -> Result<(), String> {
  #[cfg(any(target_os = "ios", target_os = "android"))]
  {
    _app
      .state::<Sensors<R>>()
      .0
      .run_mobile_plugin::<Value>("openSystemSettings", serde_json::json!({}))
      .map(|_| ())
      .map_err(|_| "Could not open Apell’s system settings.".into())
  }
  #[cfg(not(any(target_os = "ios", target_os = "android")))]
  Err("This system settings shortcut requires iOS or Android.".into())
}

/// Leave the root shell task without finishing its activity or traversing the
/// WebView's history. Only the trusted shell can request this native action.
#[cfg(target_os = "android")]
pub fn background_task<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
  app
    .state::<Sensors<R>>()
    .0
    .run_mobile_plugin::<Value>("backgroundTask", serde_json::json!({}))
    .map(|_| ())
    .map_err(|_| "Could not leave the Apell task.".into())
}

pub enum Command {
  Run,
  Cancel,
  SystemAuthorization,
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
      Command::SystemAuthorization => "systemAuthorization",
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

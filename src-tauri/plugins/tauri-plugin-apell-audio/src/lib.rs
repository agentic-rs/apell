//! Native UI and audio operations. Only the host calls this plugin, after
//! checking the applet permission and resolving a private applet file path.
use serde_json::Value;
#[cfg(any(target_os = "ios", target_os = "android"))]
use tauri::Manager;
use tauri::{AppHandle, Runtime};

#[cfg(any(target_os = "ios", target_os = "android"))]
struct Audio<R: Runtime>(tauri::plugin::PluginHandle<R>);

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_apell_audio);

pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
  tauri::plugin::Builder::new("apell-audio")
    .setup(|_app, _api| {
      #[cfg(target_os = "ios")]
      _app.manage(Audio(_api.register_ios_plugin(init_plugin_apell_audio)?));
      #[cfg(target_os = "android")]
      _app.manage(Audio(
        _api.register_android_plugin("dev.apell.audio", "AudioPlugin")?,
      ));
      Ok(())
    })
    .build()
}

pub fn available() -> bool {
  cfg!(any(target_os = "ios", target_os = "android"))
}

/// Query the shell's microphone authorization without presenting a prompt.
#[cfg(any(target_os = "ios", target_os = "android"))]
pub fn system_authorization<R: Runtime>(app: &AppHandle<R>) -> Result<String, String> {
  let result = call(app, "system_authorization", serde_json::json!({}))?;
  result
    .get("authorization")
    .and_then(Value::as_str)
    .map(str::to_owned)
    .ok_or_else(|| "Could not read microphone authorization.".into())
}

/// Invoke from a blocking worker: the system permission prompt or picker may
/// remain open until the user responds. No command is exposed to the webview.
pub fn call<R: Runtime>(
  _app: &AppHandle<R>,
  method: &str,
  _params: Value,
) -> Result<Value, String> {
  let native_method = match method {
    "start" => "start",
    "pause" => "pause",
    "resume" => "resume",
    "stop" => "stop",
    "import" => "importAudio",
    "share" => "share",
    "play" => "play",
    "playback_pause" => "playbackPause",
    "seek" => "seek",
    "status" => "status",
    "system_authorization" => "systemAuthorization",
    "reset" => "reset",
    _ => return Err("Unknown native audio operation.".into()),
  };
  #[cfg(any(target_os = "ios", target_os = "android"))]
  {
    _app
      .state::<Audio<R>>()
      .0
      .run_mobile_plugin::<Value>(native_method, _params)
      .map_err(|error| {
        // Native errors are deliberately static messages, never platform errors
        // containing local paths, device identifiers, or selected document URLs.
        match error {
          tauri::plugin::mobile::PluginInvokeError::InvokeRejected(error) => error
            .message
            .unwrap_or_else(|| "The native audio operation could not be completed.".into()),
          _ => "The native audio operation could not be completed.".into(),
        }
      })
  }
  #[cfg(not(any(target_os = "ios", target_os = "android")))]
  {
    let _ = native_method;
    Err("Native audio recording, playback, import, and sharing require iOS or Android.".into())
  }
}

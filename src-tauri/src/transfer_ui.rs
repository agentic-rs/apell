//! Trusted shell presentation for encrypted settings transfers.
use crate::{
  commands::blocking,
  host::Host,
  settings_transfer::{MAX_PAYLOAD_BYTES, MAX_QR_BYTES, TRANSFER_PREFIX},
};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::AppHandle;
use tauri_plugin_apell_sensors::Command;

const FILE_NAME: &str = "Apell.apell-settings";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
  payload: String,
}

fn validate(payload: &str, max_bytes: usize) -> Result<(), String> {
  if payload.len() > max_bytes {
    return Err(if max_bytes == MAX_QR_BYTES {
      "This transfer is too large for a QR code. Save a file or select fewer connections.".into()
    } else {
      "The settings transfer is too large.".into()
    });
  }
  let encoded = payload
    .strip_prefix(TRANSFER_PREFIX)
    .filter(|value| !value.is_empty())
    .ok_or("This is not an Apell settings transfer.")?;
  if !encoded
    .bytes()
    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
  {
    return Err("The settings transfer is invalid.".into());
  }
  Ok(())
}

pub async fn dispatch(
  app: AppHandle,
  host: &Host,
  method: &str,
  params: Value,
) -> Result<Value, String> {
  let qr = tauri_plugin_apell_sensors::available();
  if method == "settings_transfer.capabilities" {
    let capture_protection = if cfg!(target_os = "android") {
      "block"
    } else if cfg!(target_os = "ios") {
      "hide"
    } else {
      "unavailable"
    };
    return Ok(json!({ "qr": qr, "capture_protection": capture_protection }));
  }
  let _prompt = host
    .native_prompt
    .try_lock()
    .map_err(|_| "Finish the current native prompt and try again.")?;
  if method == "settings_transfer.scan_qr" {
    if !qr {
      return Err("Scan a settings QR code using an iPhone or Android device.".into());
    }
    let value =
      blocking(move || tauri_plugin_apell_sensors::call(&app, Command::ScanTransferQr, json!({})))
        .await?;
    let PayloadResult { payload } =
      serde_json::from_value(value).map_err(|_| "Could not read the settings QR code.")?;
    if let Some(payload) = &payload {
      validate(payload, MAX_QR_BYTES)?;
    }
    return Ok(json!(payload));
  }
  let Payload { payload } =
    serde_json::from_value(params).map_err(|_| "The settings transfer is invalid.")?;
  match method {
    "settings_transfer.show_qr" => {
      if !qr {
        return Err("Show a settings QR code using an iPhone or Android device.".into());
      }
      validate(&payload, MAX_QR_BYTES)?;
      blocking(move || {
        tauri_plugin_apell_sensors::call(
          &app,
          Command::ShowTransferQr,
          json!({ "payload": payload }),
        )
      })
      .await?;
      Ok(Value::Null)
    }
    "settings_transfer.save_file" => {
      validate(&payload, MAX_PAYLOAD_BYTES)?;
      save_file(app, payload).await.map(|saved| json!(saved))
    }
    _ => Err("Unknown settings transfer operation.".into()),
  }
}

#[derive(Deserialize)]
struct PayloadResult {
  payload: Option<String>,
}

#[cfg(any(target_os = "android", target_os = "ios"))]
async fn save_file(app: AppHandle, payload: String) -> Result<bool, String> {
  use tauri::Manager;
  let root = app
    .path()
    .app_cache_dir()
    .map_err(|_| "Could not access temporary storage.")?;
  blocking(move || {
    std::fs::create_dir_all(&root).map_err(|_| "Could not prepare the settings file.")?;
    let folder = tempfile::tempdir_in(root).map_err(|_| "Could not prepare the settings file.")?;
    let path = folder.path().join(FILE_NAME);
    std::fs::write(&path, payload).map_err(|_| "Could not prepare the settings file.")?;
    let path = path.to_str().ok_or("Could not access the settings file.")?;
    let response =
      tauri_plugin_apell_sensors::call(&app, Command::SaveTransferFile, json!({ "path": path }))?;
    response
      .get("saved")
      .and_then(Value::as_bool)
      .ok_or_else(|| "Could not confirm the settings file was saved.".into())
  })
  .await
}

#[cfg(target_os = "macos")]
async fn save_file(app: AppHandle, payload: String) -> Result<bool, String> {
  use objc2::MainThreadMarker;
  use objc2_app_kit::{NSModalResponseCancel, NSModalResponseOK, NSSavePanel};
  use objc2_foundation::NSString;

  let (sender, receiver) = tokio::sync::oneshot::channel();
  app
    .run_on_main_thread(move || {
      let select = || {
        let main = MainThreadMarker::new().ok_or("Could not open the save dialog.")?;
        let panel = NSSavePanel::savePanel(main);
        panel.setNameFieldStringValue(&NSString::from_str(FILE_NAME));
        let response = panel.runModal();
        if response == NSModalResponseOK {
          panel
            .URL()
            .and_then(|url| url.to_file_path())
            .map(Some)
            .ok_or("Could not access the selected folder.")
        } else if response == NSModalResponseCancel {
          Ok(None)
        } else {
          Err("Could not open the save dialog.")
        }
      };
      let _ = sender.send(select());
    })
    .map_err(|_| "Could not open the save dialog.")?;
  let Some(path) = receiver
    .await
    .map_err(|_| "The save dialog closed unexpectedly.")??
  else {
    return Ok(false);
  };
  blocking(move || {
    apell_core::bundles::atomic_write(&path, payload.as_bytes())
      .map_err(|_| "Could not save the settings file.")?;
    Ok(true)
  })
  .await
}

#[cfg(not(any(target_os = "android", target_os = "ios", target_os = "macos")))]
async fn save_file(_app: AppHandle, _payload: String) -> Result<bool, String> {
  Err("Saving a settings file is unavailable on this platform.".into())
}

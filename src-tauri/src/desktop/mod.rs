//! macOS presentation and native media adapters. Applets keep the shared bridge.
pub mod audio;
pub mod photos;
pub mod picker;
mod recording;

use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSProcessInfo, NSString, NSURL};
pub(crate) use recording::microphone_authorization;
use tauri::{AppHandle, Manager, RunEvent, Runtime, WebviewWindow, WindowEvent};

pub(crate) fn open_microphone_settings() -> Result<(), String> {
  let major = NSProcessInfo::processInfo()
    .operatingSystemVersion()
    .majorVersion;
  let pane = if major >= 13 {
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Microphone"
  } else {
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
  };
  let error = || "Open your Mac’s microphone privacy settings to update Apell’s access.".to_owned();
  let url = NSURL::URLWithString(&NSString::from_str(pane)).ok_or_else(error)?;
  NSWorkspace::sharedWorkspace()
    .openURL(&url)
    .then_some(())
    .ok_or_else(error)
}

pub fn configure_window<R: Runtime>(window: &WebviewWindow<R>) {
  let handle = window.clone();
  window.on_window_event(move |event| {
    if let WindowEvent::CloseRequested { api, .. } = event {
      // A closed window does not terminate the host or its running applets.
      // The standard Quit menu still ends the process.
      if handle.hide().is_ok() {
        api.prevent_close();
      }
    }
  });
}

pub fn on_event<R: Runtime>(app: &AppHandle<R>, event: &RunEvent) {
  if let RunEvent::Reopen { .. } = event {
    if let Some(window) = app.get_webview_window("main") {
      let _ = window.show();
      let _ = window.unminimize();
      let _ = window.set_focus();
    }
  } else if matches!(event, RunEvent::Exit) {
    if let Some(main) = objc2::MainThreadMarker::new() {
      recording::shutdown(main);
    }
  }
}

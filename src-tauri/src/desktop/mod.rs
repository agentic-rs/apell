//! macOS presentation and native media adapters. Applets keep the shared bridge.
pub mod audio;
pub mod photos;
pub mod picker;

use tauri::{AppHandle, Manager, RunEvent, Runtime, WebviewWindow, WindowEvent};

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
  }
}

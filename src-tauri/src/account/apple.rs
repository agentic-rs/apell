use super::Attempt;
use crate::host::Host;
use std::{
  ffi::{c_char, c_void, CStr, CString},
  sync::atomic::{AtomicU64, Ordering},
};
use tauri::{AppHandle, Manager, Url};

const PRESENTATION_FAILED: &str = "Could not open Apell Account sign-in.";
pub(super) const INVALID_CALLBACK: &str = "Apell Account returned an invalid sign-in response.";

struct Completion {
  app: AppHandle,
  attempt_state: String,
}

extern "C" {
  fn apell_account_present_ios(
    attempt_id: u64,
    authorization_url: *const c_char,
    context: *mut c_void,
    completion: extern "C" fn(*mut c_void, i32, *const c_char),
  );
  fn apell_account_cancel_ios(attempt_id: u64);
}

pub(super) fn next_id() -> u64 {
  static NEXT_ID: AtomicU64 = AtomicU64::new(1);
  NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub(super) fn present(app: &AppHandle, attempt: &Attempt, url: &Url) -> Result<(), &'static str> {
  let url = CString::new(url.as_str()).map_err(|_| PRESENTATION_FAILED)?;
  let context = Box::new(Completion {
    app: app.clone(),
    attempt_state: attempt.state.clone(),
  });
  // SAFETY: The bridge copies the URL before returning and returns the owned
  // context to `finished` exactly once on success, failure, or cancellation.
  unsafe {
    apell_account_present_ios(
      attempt.presentation_id,
      url.as_ptr(),
      Box::into_raw(context).cast(),
      finished,
    );
  }
  Ok(())
}

pub(super) fn cancel(attempt_id: u64) {
  // SAFETY: The bridge accepts opaque IDs and performs cancellation on the main thread.
  unsafe { apell_account_cancel_ios(attempt_id) };
}

extern "C" fn finished(context: *mut c_void, status: i32, callback_url: *const c_char) {
  // SAFETY: The bridge returns the context allocated in `present` exactly once.
  let Completion { app, attempt_state } = *unsafe { Box::from_raw(context.cast()) };
  let result = match status {
    0 if !callback_url.is_null() => {
      // SAFETY: The bridge supplies a NUL-terminated URL valid until this callback returns.
      unsafe { CStr::from_ptr(callback_url) }
        .to_str()
        .ok()
        .and_then(|url| Url::parse(url).ok())
        .map(Some)
        .ok_or(INVALID_CALLBACK)
    }
    1 => Ok(None),
    _ => Err(PRESENTATION_FAILED),
  };
  tauri::async_runtime::spawn(async move {
    app
      .state::<Host>()
      .account
      .finish_apple_sign_in(&app, &attempt_state, result)
      .await;
  });
}

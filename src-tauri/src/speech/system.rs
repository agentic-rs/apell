use serde_json::Value;
use std::{path::Path, sync::atomic::AtomicBool};

#[cfg(target_vendor = "apple")]
mod apple {
  use super::*;
  use std::{
    ffi::{c_char, c_void, CStr, CString},
    ptr,
    sync::atomic::Ordering,
  };

  extern "C" {
    fn apell_system_speech_unavailable_reason() -> *const c_char;
    fn apell_system_transcribe(
      path: *const c_char,
      language: *const c_char,
      cancelled: extern "C" fn(*const c_void) -> bool,
      context: *const c_void,
    ) -> *mut c_char;
    fn apell_free_speech_reply(reply: *mut c_char);
  }

  extern "C" fn cancelled(context: *const c_void) -> bool {
    // Only called synchronously by the worker before apell_system_transcribe returns.
    unsafe { &*context.cast::<AtomicBool>() }.load(Ordering::SeqCst)
  }

  pub(super) fn unavailable_reason() -> Option<&'static str> {
    let reason = unsafe { apell_system_speech_unavailable_reason() };
    if reason.is_null() {
      return None;
    }
    // The native function returns only static UTF-8 messages.
    Some(
      unsafe { CStr::from_ptr(reason) }
        .to_str()
        .unwrap_or("System speech is unavailable."),
    )
  }

  pub(super) fn transcribe(
    path: &Path,
    language: Option<&str>,
    cancel: &AtomicBool,
  ) -> Result<Value, String> {
    let path =
      CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| "Invalid audio path.")?;
    let language = language
      .map(CString::new)
      .transpose()
      .map_err(|_| "Invalid language.")?;
    let reply = unsafe {
      apell_system_transcribe(
        path.as_ptr(),
        language.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        cancelled,
        ptr::from_ref(cancel).cast(),
      )
    };
    if reply.is_null() {
      return Err("System speech recognition is unavailable.".into());
    }
    // Native code owns the allocation and returns either a transcript object or a static error message.
    let bytes = unsafe { CStr::from_ptr(reply) }.to_bytes();
    let result = if bytes.starts_with(b"{") {
      serde_json::from_slice(bytes).map_err(|_| "Could not read the system transcript.".into())
    } else {
      Err(String::from_utf8_lossy(bytes).into_owned())
    };
    unsafe { apell_free_speech_reply(reply) };
    result
  }
}

pub(super) fn unavailable_reason() -> Option<&'static str> {
  #[cfg(target_vendor = "apple")]
  {
    apple::unavailable_reason()
  }
  #[cfg(not(target_vendor = "apple"))]
  {
    Some("System file recognition requires iOS or macOS.")
  }
}

pub(super) fn transcribe(
  path: &Path,
  language: Option<&str>,
  cancel: &AtomicBool,
) -> Result<Value, String> {
  #[cfg(target_vendor = "apple")]
  {
    apple::transcribe(path, language, cancel)
  }
  #[cfg(not(target_vendor = "apple"))]
  {
    let _ = (path, language, cancel);
    Err("System file transcription is unavailable on this platform.".into())
  }
}

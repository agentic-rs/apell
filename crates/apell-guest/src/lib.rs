//! Guest side of Apell's native backend ABI. Futures live in guest memory and
//! are polled on serial VM turns, independently of any attached frontend.
mod executor;
pub mod protocol;
pub mod services;

pub use executor::{call, call_with_events, emit, spawn};
pub use protocol::HostError;
use protocol::{Inbound, Outbound, MAX_MESSAGE_BYTES};
use serde_json::Value;
use std::future::Future;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "apell")]
unsafe extern "C" {
  #[link_name = "send"]
  fn host_send(ptr: *const u8, len: u32) -> i32;
  fn host_value(kind: i32, ptr: *mut u8, capacity: u32) -> i32;
}

fn send(record: &Outbound) -> Result<(), HostError> {
  let bytes = serde_json::to_vec(record).map_err(HostError::new)?;
  if bytes.len() > MAX_MESSAGE_BYTES {
    return Err(HostError::new(
      "The backend message exceeds the ABI size limit.",
    ));
  }
  #[cfg(target_arch = "wasm32")]
  if unsafe { host_send(bytes.as_ptr(), bytes.len() as u32) } == 0 {
    return Ok(());
  }
  Err(HostError::new(
    "The native host did not accept the backend message.",
  ))
}

fn native_value(kind: i32) -> String {
  #[cfg(target_arch = "wasm32")]
  {
    let mut bytes = [0u8; 64];
    let len = unsafe { host_value(kind, bytes.as_mut_ptr(), bytes.len() as u32) };
    let bytes = usize::try_from(len)
      .ok()
      .and_then(|len| bytes.get(..len))
      .expect("invalid length from Apell host_value");
    std::str::from_utf8(bytes)
      .expect("invalid UTF-8 from Apell host_value")
      .to_owned()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    let _ = kind;
    panic!("host_value is available only inside an Apell backend")
  }
}

pub fn create_id() -> String {
  native_value(1)
}
pub fn now() -> String {
  native_value(2)
}

#[doc(hidden)]
pub fn abi_alloc(len: u32) -> *mut u8 {
  if len == 0 || len as usize > MAX_MESSAGE_BYTES {
    return std::ptr::null_mut();
  }
  Box::into_raw(vec![0u8; len as usize].into_boxed_slice()).cast::<u8>()
}

#[doc(hidden)]
/// # Safety
/// The host must pass the pointer and length of an existing ABI allocation exactly once.
pub unsafe fn abi_free(ptr: *mut u8, len: u32) {
  let slice = std::ptr::slice_from_raw_parts_mut(ptr, len as usize);
  drop(unsafe { Box::from_raw(slice) });
}

#[doc(hidden)]
/// # Safety
/// The host must have initialized these bytes in an existing ABI allocation.
pub unsafe fn abi_dispatch<F, Fut>(ptr: *const u8, len: u32, handle: F) -> i32
where
  F: FnOnce(String, Value) -> Fut + 'static,
  Fut: Future<Output = Result<Value, HostError>> + 'static,
{
  if ptr.is_null() || len == 0 || len as usize > MAX_MESSAGE_BYTES {
    return -1;
  }
  let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
  let Ok(input) = serde_json::from_slice::<Inbound>(bytes) else {
    return -1;
  };
  if executor::dispatch(input, handle).is_ok() {
    0
  } else {
    -1
  }
}

/// Export an async command handler from a wasm32-unknown-unknown cdylib.
#[macro_export]
macro_rules! export {
  ($handler:path) => {
    #[cfg(target_arch = "wasm32")]
    #[unsafe(no_mangle)]
    pub extern "C" fn apell_alloc(len: u32) -> *mut u8 {
      $crate::abi_alloc(len)
    }
    #[cfg(target_arch = "wasm32")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn apell_free(ptr: *mut u8, len: u32) {
      unsafe { $crate::abi_free(ptr, len) }
    }
    #[cfg(target_arch = "wasm32")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn apell_dispatch(ptr: *const u8, len: u32) -> i32 {
      unsafe { $crate::abi_dispatch(ptr, len, $handler) }
    }
  };
}

//! Generic backend-only host capabilities. Every call is permission checked by
//! the native host and scoped to this verified applet run, never to its view.
use crate::{call, HostError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Serialize)]
pub struct Header {
  pub name: String,
  pub value: String,
}

#[derive(Deserialize)]
pub struct Socket {
  pub socket_handle: String,
}

#[derive(Deserialize)]
struct Received {
  data: Option<String>,
}
#[derive(Deserialize)]
struct Secret {
  value: Option<String>,
}
#[derive(Deserialize)]
struct Random {
  data: String,
}

impl Socket {
  pub async fn open(url: &str, headers: &[Header]) -> Result<Self, HostError> {
    call("websocket.open", json!({"url":url,"headers":headers})).await
  }
  /// Unpadded base64url, bounded to 1 MiB decoded bytes. Never replayed on error.
  pub async fn send(&self, data: &str) -> Result<(), HostError> {
    let _: Value = call(
      "websocket.send",
      json!({"socket_handle":self.socket_handle,"data":data}),
    )
    .await?;
    Ok(())
  }
  /// None is a clean close. A timeout or abnormal close is an error and retires
  /// the socket. One receive may be pending on a socket at a time.
  pub async fn receive(&self, timeout_ms: u64) -> Result<Option<String>, HostError> {
    let result: Received = call(
      "websocket.receive",
      json!({"socket_handle":self.socket_handle,"timeout_ms":timeout_ms}),
    )
    .await?;
    Ok(result.data)
  }
  pub async fn close(&self) -> Result<(), HostError> {
    let _: Value = call(
      "websocket.close",
      json!({"socket_handle":self.socket_handle}),
    )
    .await?;
    Ok(())
  }
}

pub async fn secret(key: &str) -> Result<Option<String>, HostError> {
  let result: Secret = call("secrets.get", json!({"key":key})).await?;
  Ok(result.value)
}
pub async fn save_secret(key: &str, value: &str) -> Result<(), HostError> {
  let _: Value = call("secrets.set", json!({"key":key,"value":value})).await?;
  Ok(())
}
pub async fn delete_secret(key: &str) -> Result<(), HostError> {
  let _: Value = call("secrets.delete", json!({"key":key})).await?;
  Ok(())
}
/// Returns cryptographically secure bytes as unpadded base64url. The guest may
/// seed a private cryptographic RNG; UUIDs and time are not entropy sources.
pub async fn random_bytes(length: u32) -> Result<String, HostError> {
  let result: Random = call("crypto.random_bytes", json!({"length":length})).await?;
  Ok(result.data)
}
pub async fn sleep(duration_ms: u64) -> Result<(), HostError> {
  let _: Value = call("runtime.sleep", json!({"duration_ms":duration_ms})).await?;
  Ok(())
}

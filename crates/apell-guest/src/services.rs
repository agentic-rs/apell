//! Generic backend-only host capabilities. Every call is permission checked by
//! the native host and scoped to this verified applet run, never to its view.
use crate::{call, HostError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Maximum decoded length of a binary WebSocket record.
pub const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_RECORD_CHARS: usize = (MAX_RECORD_BYTES * 4).div_ceil(3);
const INVALID_DIGIT: u8 = u8::MAX;
const DIGITS: [u8; 256] = {
  let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
  let mut digits = [INVALID_DIGIT; 256];
  let mut index = 0;
  while index < alphabet.len() {
    digits[alphabet[index] as usize] = index as u8;
    index += 1;
  }
  digits
};

fn check_record_size(data: &str) -> Result<(), HostError> {
  if data.len() > MAX_RECORD_CHARS {
    Err(HostError::new("WebSocket record exceeds 1 MiB."))
  } else {
    Ok(())
  }
}

// A maximum-size base64 string can exhaust a Wasm turn in serde_json's
// general-purpose string escaper. Validate the alphabet once and copy the
// record as a single JSON string fragment. The other strings still use serde
// escaping. This encoder serves both Socket::send and raw guest calls.
pub(super) fn encode_socket_send(request_id: &str, params: &Value) -> Result<Vec<u8>, HostError> {
  let fields = params
    .as_object()
    .ok_or_else(|| HostError::new("Invalid WebSocket parameters."))?;
  if !fields.get("socket_handle").is_some_and(Value::is_string) {
    return Err(HostError::new("Missing WebSocket handle."));
  }
  let data = fields
    .get("data")
    .and_then(Value::as_str)
    .ok_or_else(|| HostError::new("Missing WebSocket record."))?;
  check_record_size(data)?;
  let bytes = data.as_bytes();
  let last = bytes.last().map_or(0, |&byte| DIGITS[byte as usize]);
  let valid_tail = match bytes.len() % 4 {
    0 => true,
    2 => last & 15 == 0,
    3 => last & 3 == 0,
    _ => false,
  };
  // Classify each four-digit base64 group with a single branch. This leaves
  // enough fuel to validate a full record even in the turn that received it.
  let (groups, tail) = bytes.as_chunks::<4>();
  let invalid_group = |&[a, b, c, d]: &[u8; 4]| {
    DIGITS[a as usize] | DIGITS[b as usize] | DIGITS[c as usize] | DIGITS[d as usize]
      == INVALID_DIGIT
  };
  if !valid_tail
    || groups.iter().any(invalid_group)
    || tail.iter().any(|&b| DIGITS[b as usize] == INVALID_DIGIT)
  {
    return Err(HostError::new(
      "WebSocket data must be canonical unpadded base64url.",
    ));
  }
  let mut wire = Vec::with_capacity(bytes.len() + 192);
  wire.extend_from_slice(br#"{"kind":"host_call","method":"websocket.send","request_id":"#);
  serde_json::to_writer(&mut wire, request_id).map_err(HostError::new)?;
  wire.extend_from_slice(br#","params":{"data":""#);
  wire.extend_from_slice(bytes);
  wire.push(b'"');
  for (name, value) in fields {
    if name != "data" {
      wire.push(b',');
      serde_json::to_writer(&mut wire, name).map_err(HostError::new)?;
      wire.push(b':');
      serde_json::to_writer(&mut wire, value).map_err(HostError::new)?;
    }
  }
  wire.extend_from_slice(b"}}");
  Ok(wire)
}

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
    check_record_size(data)?;
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::protocol::Outbound;

  #[test]
  fn socket_encoder_preserves_the_host_call_envelope_and_parameter_values() {
    let request_id = "test\"id\\";
    let params = json!({
      "socket_handle":"escaped\\handle",
      "data":"-_8",
      "extra\"field":{"nested":["quote\"", null]},
    });
    let wire = encode_socket_send(request_id, &params).unwrap();
    let expected = Outbound::HostCall {
      request_id: request_id.into(),
      method: "websocket.send".into(),
      params,
    };
    assert_eq!(
      serde_json::from_slice::<Value>(&wire).unwrap(),
      serde_json::to_value(expected).unwrap()
    );
  }

  #[test]
  fn socket_encoder_accepts_only_bounded_canonical_records() {
    let record =
      |data: &str| encode_socket_send("id", &json!({"socket_handle":"handle","data":data}));
    for valid in ["", "AA", "AAA", "AAAA", "-_8", "_w"] {
      assert!(record(valid).is_ok(), "{valid:?}");
    }
    for invalid in [
      "A", "AB", "AAB", "AA==", "AQI=", "AA+/", "AA\"A", "AA\\A", "AA\nA", "AAé",
    ] {
      assert!(record(invalid).is_err(), "{invalid:?}");
    }
    let oversized = "A".repeat(MAX_RECORD_CHARS + 1);
    assert!(record(&oversized)
      .unwrap_err()
      .message
      .contains("exceeds 1 MiB"));
    for params in [
      Value::Null,
      json!([]),
      json!({"data":"AA"}),
      json!({"socket_handle":42,"data":"AA"}),
      json!({"socket_handle":"handle"}),
      json!({"socket_handle":"handle","data":["AA"]}),
    ] {
      assert!(encode_socket_send("id", &params).is_err(), "{params}");
    }
  }
}

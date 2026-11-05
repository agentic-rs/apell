//! Generic run-owned services. The UI bridge cannot access these operations.
use crate::host::{string, Host, Session};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::AppHandle;

pub async fn dispatch(
  app: &AppHandle,
  host: &Host,
  owner: &str,
  session: Session,
  method: &str,
  params: Value,
) -> Result<Value, String> {
  if !session.alive.load(std::sync::atomic::Ordering::SeqCst) {
    return Err("Applet backend is closed.".into());
  }
  match method {
    "websocket.open" => {
      host
        .websocket
        .open(
          owner,
          serde_json::from_value(params).map_err(|_| "Invalid WebSocket parameters.")?,
          session.cancellation.subscribe(),
        )
        .await
    }
    "websocket.send" => {
      host
        .websocket
        .send(
          owner,
          string(&params, "socket_handle")?,
          string(&params, "data")?,
        )
        .await
    }
    "websocket.receive" => {
      let timeout_ms = match params.get("timeout_ms") {
        Some(value) => value.as_u64().ok_or("Invalid WebSocket timeout.")?,
        None => 90_000,
      };
      host
        .websocket
        .receive(owner, string(&params, "socket_handle")?, timeout_ms)
        .await
    }
    "websocket.close" => host
      .websocket
      .close(owner, string(&params, "socket_handle")?),
    "crypto.random_bytes" => {
      let length = params
        .get("length")
        .and_then(Value::as_u64)
        .filter(|length| (1..=4096).contains(length))
        .ok_or("Random byte length must be between 1 and 4096.")?;
      let mut bytes = vec![0; length as usize];
      getrandom::fill(&mut bytes).map_err(|_| "Secure randomness is unavailable.")?;
      Ok(json!({"data":URL_SAFE_NO_PAD.encode(bytes)}))
    }
    "runtime.sleep" => {
      let duration = params
        .get("duration_ms")
        .and_then(Value::as_u64)
        .filter(|duration| (1..=120_000).contains(duration))
        .ok_or("Sleep duration must be between 1 and 120000 milliseconds.")?;
      let mut cancellation = session.cancellation.subscribe();
      if *cancellation.borrow() {
        return Err("Applet backend is closed.".into());
      }
      tokio::select! {
        biased;
        _ = cancellation.changed() => Err("Applet backend is closed.".into()),
        _ = tokio::time::sleep(std::time::Duration::from_millis(duration)) => Ok(Value::Null),
      }
    }
    "secrets.get" | "secrets.set" | "secrets.delete" => {
      let account = secret_account(&session.manifest.applet_id, string(&params, "key")?)?;
      let value = if method == "secrets.set" {
        let value = string(&params, "value")?;
        if value.len() > 16 * 1024 {
          return Err("Applet secret exceeds 16 KiB.".into());
        }
        Some(value.to_owned())
      } else {
        None
      };
      let method = method.to_owned();
      let app = app.clone();
      let _gate = host.credential_gate.lock().await;
      crate::commands::blocking(move || {
        session.run_local(|| {
          if method == "secrets.get" {
            Ok(json!({"value":tauri_plugin_apell_credentials::read(&app, &account)?}))
          } else {
            tauri_plugin_apell_credentials::write(&app, &account, value.as_deref())?;
            Ok(Value::Null)
          }
        })
      })
      .await
    }
    _ => Err("Unknown backend service operation.".into()),
  }
}

fn secret_account(applet_id: &str, key: &str) -> Result<String, String> {
  if key.is_empty()
    || key.len() > 128
    || !key
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
  {
    return Err(
      "Secret keys require 1–128 ASCII letters, digits, dots, underscores or hyphens.".into(),
    );
  }
  // The verified applet identity is host-owned, not accepted from guest params.
  // The platform plugin accepts opaque SHA-256 account IDs on Android. Domain
  // separation prevents collisions with the shell's provider connection slots.
  Ok(format!(
    "{:x}",
    Sha256::digest(format!("apell-applet-secret-v1:{applet_id}:{key}"))
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn secret_slots_cannot_escape_the_verified_applet_namespace() {
    let slot = secret_account("one", "identity").unwrap();
    assert_eq!(slot.len(), 64);
    assert!(slot.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(
      secret_account("one", "identity").unwrap(),
      secret_account("two", "identity").unwrap()
    );
    for key in ["", ":another:identity", "../key", "with space", "\n"] {
      assert!(secret_account("one", key).is_err());
    }
    assert!(secret_account("one", &"a".repeat(129)).is_err());
  }
}

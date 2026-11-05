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
    "websocket.close" => {
      host
        .websocket
        .close(owner, string(&params, "socket_handle")?)
        .await
    }
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
      with_applet_vault(host, session, move || {
        if method == "secrets.get" {
          Ok(json!({"value":tauri_plugin_apell_credentials::read(&app, &account)?}))
        } else {
          tauri_plugin_apell_credentials::write(&app, &account, value.as_deref())?;
          Ok(Value::Null)
        }
      })
      .await
    }
    _ => Err("Unknown backend service operation.".into()),
  }
}

async fn with_applet_vault(
  host: &Host,
  session: Session,
  operation: impl FnOnce() -> Result<Value, String> + Send + 'static,
) -> Result<Value, String> {
  // Provider refreshes may wait on the network. Apple stores each account in
  // its own Keychain item. Android runs synchronous vault commands serially
  // on the Tauri Android context, including creation of its shared Keystore key.
  let gate = host.applet_secret_gate.clone().lock_owned().await;
  crate::commands::blocking(move || {
    // The worker can outlive its waiter when the guest traps. Keep the lock
    // through the platform call and recheck the session after queueing.
    let _guard = gate;
    session.run_local(operation)
  })
  .await
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
mod tests;

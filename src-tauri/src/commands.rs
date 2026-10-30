use crate::host::{credential_account, public_config, string, Host, Session};
use apell_core::{
  bundles::{sandbox_document, MAX_BUNDLE_BYTES},
  llm::{validate_config, LlmConfig, LlmRequest},
};
use serde_json::{json, Value};
use std::sync::{
  atomic::{AtomicBool, Ordering},
  Arc,
};
use tauri::{AppHandle, Emitter, Manager, State};

#[tauri::command(rename_all = "snake_case")]
pub async fn shell_command(
  app: AppHandle,
  host: State<'_, Host>,
  host_key: String,
  method: String,
  params: Value,
) -> Result<Value, String> {
  host.authorize(&host_key)?;
  match method.as_str() {
    "applets.list" => {
      let store = host.bundles.clone();
      blocking(move || {
        serde_json::to_value(store.list()?).map_err(|_| "Could not encode applets.".into())
      })
      .await
    }
    "applets.install" => {
      let expected_hash = string(&params, "sha256")?.to_owned();
      let bytes = if let Some(bundle) = params.get("bundle").and_then(Value::as_str) {
        bundle.as_bytes().to_vec()
      } else {
        let url = string(&params, "url")?;
        let app = app.clone();
        host
          .resources
          .fetch(url, MAX_BUNDLE_BYTES, move |received_bytes, total_bytes| {
            let _ = app.emit_to(
              "main",
              "install-progress",
              json!({"received_bytes":received_bytes,"total_bytes":total_bytes}),
            );
          })
          .await?
      };
      let store = host.bundles.clone();
      blocking(move || {
        serde_json::to_value(store.install(&bytes, &expected_hash)?)
          .map_err(|_| "Could not encode installed applet.".into())
      })
      .await
    }
    "applets.launch" => {
      let applet_id = string(&params, "applet_id")?.to_owned();
      let store = host.bundles.clone();
      let bundle = tauri::async_runtime::spawn_blocking(move || store.load(&applet_id))
        .await
        .map_err(|_| "Could not launch applet.")??;
      let previous: Vec<String> = host
        .sessions
        .lock()
        .map_err(|_| "Session state unavailable.")?
        .keys()
        .cloned()
        .collect();
      for session_id in previous {
        let app = app.clone();
        blocking(move || {
          app.state::<Host>().close(&session_id)?;
          Ok(Value::Null)
        })
        .await?;
      }
      let session_id = uuid::Uuid::new_v4().to_string();
      host
        .sqlite
        .register_session(&session_id, &bundle.manifest.applet_id)?;
      if let Err(error) = host.llm.register_session(&session_id) {
        host.sqlite.close_session(&session_id);
        return Err(error);
      }
      host
        .sessions
        .lock()
        .map_err(|_| "Session state unavailable.")?
        .insert(
          session_id.clone(),
          Session {
            manifest: bundle.manifest.clone(),
            alive: Arc::new(AtomicBool::new(true)),
            cancellation: tokio::sync::watch::channel(false).0,
          },
        );
      Ok(
        json!({"session_id":session_id,"manifest":bundle.manifest,"html":sandbox_document(&bundle.html)}),
      )
    }
    "applets.close" => {
      let session_id = string(&params, "session_id")?.to_owned();
      let app = app.clone();
      blocking(move || {
        app.state::<Host>().close(&session_id)?;
        Ok(Value::Null)
      })
      .await
    }
    "llm.settings" => {
      let config = host.settings.lock().await;
      let mut value = serde_json::to_value(&*config).map_err(|_| "Could not encode settings.")?;
      let account = credential_account(&config);
      let is_demo = config.base_url == "demo";
      value["has_api_key"] = if is_demo {
        json!(false)
      } else {
        blocking(move || {
          Ok(json!(
            tauri_plugin_apell_credentials::read(&app, &account)?.is_some()
          ))
        })
        .await?
      };
      Ok(value)
    }
    "llm.save_settings" => {
      let config: LlmConfig =
        serde_json::from_value(params.get("config").cloned().ok_or("Missing config.")?)
          .map_err(|_| "Invalid LLM settings.")?;
      validate_config(&config)?;
      let key = params
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::to_owned);
      if key
        .as_ref()
        .is_some_and(|k| k.len() > 8192 || k.chars().any(char::is_control))
      {
        return Err("Invalid API key.".into());
      }
      let mut current = host.settings.lock().await;
      let account = credential_account(&config);
      let saved_config = config.clone();
      blocking(move || {
        let changes_key = key.is_some() && saved_config.base_url != "demo";
        let previous = if changes_key { tauri_plugin_apell_credentials::read(&app, &account)? } else { None };
        if changes_key {
          let key = key.as_deref().filter(|key| !key.is_empty());
          tauri_plugin_apell_credentials::write(&app, &account, key)?;
        }
        if let Err(error) = app.state::<Host>().save_settings(&saved_config) {
          if changes_key && tauri_plugin_apell_credentials::write(&app, &account, previous.as_deref()).is_err() {
            return Err("Settings could not be saved and the previous credential could not be restored. Re-enter the key.".into());
          }
          return Err(error);
        }
        Ok(Value::Null)
      }).await?;
      *current = config;
      Ok(Value::Null)
    }
    _ => Err("Unknown shell operation.".into()),
  }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn applet_call(
  app: AppHandle,
  host: State<'_, Host>,
  host_key: String,
  session_id: String,
  method: String,
  params: Value,
) -> Result<Value, String> {
  host.authorize(&host_key)?;
  let session = host.session(&session_id, &method)?;
  if method.starts_with("sqlite.") {
    let sqlite = host.sqlite.clone();
    return blocking(move || {
      if !session.alive.load(Ordering::SeqCst) {
        return Err("Applet session is closed.".into());
      }
      sqlite.dispatch(&session_id, &session.manifest.applet_id, &method, params)
    })
    .await;
  }
  match method.as_str() {
    "llm.config" => Ok(public_config(&*host.settings.lock().await)),
    "llm.complete" => {
      let request: LlmRequest =
        serde_json::from_value(params).map_err(|_| "Invalid LLM request.")?;
      let current = host.settings.lock().await;
      let config = current.clone();
      let account = credential_account(&config);
      let app_for_key = app.clone();
      let key = if config.base_url == "demo" {
        None
      } else {
        tauri::async_runtime::spawn_blocking(move || {
          tauri_plugin_apell_credentials::read(&app_for_key, &account)
        })
        .await
        .map_err(|_| "Credential read interrupted.")??
      };
      drop(current);
      if !session.alive.load(Ordering::SeqCst) {
        return Err("Applet session is closed.".into());
      }
      let event_session = session_id.clone();
      host
        .llm
        .complete(&session_id, request, config, key, move |event| {
          let _ = app.emit_to(
            "main",
            "applet-event",
            json!({"session_id":event_session,"event":event}),
          );
        })
        .await
    }
    "llm.cancel" => {
      host
        .llm
        .cancel(&session_id, string(&params, "request_id")?)?;
      Ok(Value::Null)
    }
    "resources.download" => {
      let request_id = string(&params, "request_id")?.to_owned();
      let event_session = session_id.clone();
      let mut cancellation = session.cancellation.subscribe();
      if *cancellation.borrow() {
        return Err("Applet session is closed.".into());
      }
      let download = host.resources.download(&session.manifest.applet_id, string(&params,"url")?, params.get("sha256").and_then(Value::as_str), move |received_bytes,total_bytes| {
        let _ = app.emit_to("main", "applet-event", json!({"session_id":event_session,"event":{"request_id":request_id,"kind":"download_progress","received_bytes":received_bytes,"total_bytes":total_bytes}}));
      });
      tokio::select! {
        biased;
        _ = cancellation.changed() => Err("Applet session is closed.".into()),
        result = download => result,
      }
    }
    "resources.read" => {
      host
        .resources
        .read(
          &session.manifest.applet_id,
          string(&params, "resource_id")?,
          params.get("offset").and_then(Value::as_u64).unwrap_or(0),
          params
            .get("length")
            .and_then(Value::as_u64)
            .unwrap_or(65536)
            .min(usize::MAX as u64) as usize,
        )
        .await
    }
    "device.info" => Ok(json!({"platform":std::env::consts::OS,"api_version":1})),
    _ => Err("Unknown applet operation.".into()),
  }
}

async fn blocking(
  task: impl FnOnce() -> Result<Value, String> + Send + 'static,
) -> Result<Value, String> {
  tauri::async_runtime::spawn_blocking(task)
    .await
    .map_err(|_| "Host operation was interrupted.".to_string())?
}

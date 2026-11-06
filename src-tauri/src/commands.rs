use crate::host::{public_config, string, Host, NETWORK_DISABLED};
use crate::provider_commands::{snapshot, ModelRoute, ModelSnapshot};
use crate::settings::Settings;
use apell_core::llm::{
  deserialize_target, LlmConfig, LlmEvent, LlmFailure, LlmProvenance, LlmRequest, LlmResult,
  LlmStatus, LlmTarget,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigQuery {
  #[serde(default, deserialize_with = "deserialize_target")]
  target: Option<LlmTarget>,
}

fn failed_generation(
  request_id: String,
  provenance: LlmProvenance,
  error: LlmFailure,
) -> Result<Value, String> {
  serde_json::to_value(LlmResult {
    request_id,
    status: LlmStatus::Failed,
    content: String::new(),
    tool_calls: Vec::new(),
    message: None,
    finish_reason: None,
    provenance,
    error: Some(error),
  })
  .map_err(|_| "Could not encode LLM result.".into())
}

enum OfflineModel {
  Demo(LlmConfig),
  Remote(LlmProvenance),
}

fn resolve_offline_model(
  route: ModelRoute<'_>,
  settings: &Settings,
) -> Result<OfflineModel, String> {
  let config = route.resolve(settings)?.config();
  // Match the condition that selects the local implementation in LlmService.
  if config.base_url == "demo" {
    Ok(OfflineModel::Demo(config))
  } else {
    Ok(OfflineModel::Remote(config.provenance()))
  }
}

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
        serde_json::to_value(store.list_report()?).map_err(|_| "Could not encode applets.".into())
      })
      .await
    }
    "applets.install" => crate::install::install(app, &host, params).await,
    "applets.update" => crate::install::update(app, &host, string(&params, "applet_id")?).await,
    "applets.github_release" => crate::install::release(string(&params, "repository")?).await,
    "applets.current" => {
      let _gate = host.run_gate.lock().await;
      serde_json::to_value(host.resume_view()?)
        .map_err(|_| "Could not encode applet attachment.".into())
    }
    "applets.reload" => serde_json::to_value(
      host.attach_view(string(&params, "session_id")?, string(&params, "view_id")?)?,
    )
    .map_err(|_| "Could not encode applet attachment.".into()),
    "applets.launch" => {
      let attachment = crate::applets::launch(app, string(&params, "applet_id")?).await?;
      serde_json::to_value(attachment).map_err(|_| "Could not encode applet attachment.".into())
    }
    "applets.close" => {
      host.detach_view(string(&params, "session_id")?, string(&params, "view_id")?)?;
      Ok(Value::Null)
    }
    "permissions.list" => crate::permissions::list(&host).await,
    "permissions.forget_microphone" => {
      let applet_id = string(&params, "applet_id")?.to_owned();
      blocking(move || {
        app.state::<Host>().forget_microphone(&applet_id)?;
        Ok(Value::Null)
      })
      .await
    }
    method if method.starts_with("llm.") => {
      crate::provider_commands::dispatch(app, &host, method, params).await
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
  view_id: String,
  method: String,
  params: Value,
) -> Result<Value, String> {
  host.authorize(&host_key)?;
  host.require_view(&session_id, &view_id)?;
  if method == "view.check" {
    return Ok(Value::Null);
  }
  if method == "backend.invoke" {
    return host
      .invoke_backend(
        &session_id,
        string(&params, "command")?,
        params.get("params").cloned().unwrap_or_else(|| json!({})),
      )
      .await
      .map_err(|error| error.message);
  }
  service_call(app, view_id, method, params, None).await
}

// Native UI and speech work belong to an attached view, whose cancellation
// signal fires on close/reload. Persistent backend runs outlive that signal.
fn require_view_for_media(method: &str, from_backend: bool) -> Result<(), String> {
  if from_backend && matches!(method.split('.').next(), Some("audio" | "speech")) {
    Err("Audio and speech services are available only to an attached applet view.".into())
  } else {
    Ok(())
  }
}

pub async fn service_call(
  app: AppHandle,
  session_id: String,
  method: String,
  params: Value,
  backend_events: Option<apell_runtime::HostEvents>,
) -> Result<Value, String> {
  let host = app.state::<Host>();
  let session = host.session(&session_id, &method)?;
  require_view_for_media(&method, backend_events.is_some())?;
  if matches!(
    method.split('.').next(),
    Some("websocket" | "secrets" | "crypto" | "runtime")
  ) {
    if backend_events.is_none() {
      return Err("This host service is available only to an applet backend.".into());
    }
    return crate::backend_services::dispatch(&app, &host, &session_id, session, &method, params)
      .await;
  }
  if method.starts_with("audio.") {
    return crate::media::dispatch(app.clone(), &host, &session_id, session, &method, params).await;
  }
  if method.starts_with("speech.") {
    return crate::speech::dispatch(&host, &session_id, session, &method, params).await;
  }
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
  if method.starts_with("fs.") {
    let files = host.files.clone();
    return blocking(move || {
      session.run_local(|| files.dispatch(&session.manifest.applet_id, &method, params))
    })
    .await;
  }
  if method.starts_with("config.") {
    let config = host.config.clone();
    return blocking(move || {
      session.run_local(|| config.dispatch(&session.manifest.applet_id, &method, params))
    })
    .await;
  }
  match method.as_str() {
    "http.request" => {
      let request =
        serde_json::from_value(params).map_err(|_| "Invalid HTTP request parameters.")?;
      let mut cancellation = session.cancellation.subscribe();
      if *cancellation.borrow() {
        return Err("Applet session is closed.".into());
      }
      tokio::select! {
        biased;
        _ = cancellation.changed() => Err("Applet session is closed.".into()),
        response = host.http.request(request) => serde_json::to_value(response?).map_err(|_| "Could not encode HTTP response.".into()),
      }
    }
    "llm.config" => {
      let query: ConfigQuery =
        serde_json::from_value(params).map_err(|_| "Invalid model selection.")?;
      let connection =
        ModelRoute::applet(query.target.as_ref()).resolve(&*host.settings.lock().await)?;
      Ok(public_config(&connection.config()))
    }
    "llm.generate" => {
      let request: LlmRequest =
        serde_json::from_value(params).map_err(|_| "Invalid LLM request.")?;
      let route = ModelRoute::applet(request.target.as_ref());
      let selected = if session.manifest.network {
        snapshot(&app, &host, route).await
      } else {
        // Offline calls must never enter snapshot, which can refresh credentials
        // over the network. Resolve once against the saved settings.
        match resolve_offline_model(route, &*host.settings.lock().await) {
          Ok(OfflineModel::Demo(config)) => Ok(ModelSnapshot {
            config,
            account: Ok(None),
          }),
          Ok(OfflineModel::Remote(provenance)) => {
            return failed_generation(
              request.request_id,
              provenance,
              LlmFailure {
                code: "network_disabled".into(),
                message: NETWORK_DISABLED.into(),
                capability: None,
                retryable: false,
              },
            );
          }
          Err(message) => Err(message),
        }
      };
      if !session.alive.load(Ordering::SeqCst) {
        return Err("Applet session is closed.".into());
      }
      let event_session = session_id.clone();
      let event_app = app.clone();
      let emit = move |event: LlmEvent| {
        if let Some(events) = &backend_events {
          if let Ok(event) = serde_json::to_value(event) {
            let _ = events.emit(event);
          }
        } else {
          let _ = event_app.emit_to(
            "main",
            "applet-event",
            json!({"session_id":event_session,"event":event}),
          );
        }
      };
      let ModelSnapshot { config, account } = match selected {
        Ok(snapshot) => snapshot,
        Err(message) => {
          let target = request.target.as_ref().ok_or_else(|| message.clone())?;
          return failed_generation(
            request.request_id,
            target.provenance(),
            LlmFailure {
              code: "invalid_target".into(),
              message,
              capability: None,
              retryable: false,
            },
          );
        }
      };
      let key = match account {
        Ok(account) => account,
        Err(_) => {
          return failed_generation(
            request.request_id,
            config.provenance(),
            LlmFailure {
              code: "credential_unavailable".into(),
              message: "The provider credential is unavailable or needs sign-in. Check Settings."
                .into(),
              capability: None,
              retryable: true,
            },
          )
        }
      };
      let result = host
        .llm
        .generate(&session_id, request, config, key, emit)
        .await;
      serde_json::to_value(result).map_err(|_| "Could not encode LLM result.".into())
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
      let event_app = app.clone();
      let mut cancellation = session.cancellation.subscribe();
      if *cancellation.borrow() {
        return Err("Applet session is closed.".into());
      }
      let download = host.resources.download(&session.manifest.applet_id, string(&params,"url")?, params.get("sha256").and_then(Value::as_str), move |received_bytes,total_bytes| {
        let event = json!({"request_id":request_id,"kind":"download_progress","received_bytes":received_bytes,"total_bytes":total_bytes});
        if let Some(events) = &backend_events { let _ = events.emit(event); }
        else { let _ = event_app.emit_to("main", "applet-event", json!({"session_id":event_session,"event":event})); }
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
    "device.info" => {
      Ok(json!({"platform":std::env::consts::OS,"api_version":apell_core::bundles::API_VERSION}))
    }
    _ => Err("Unknown applet operation.".into()),
  }
}

pub(crate) async fn blocking<T: Send + 'static>(
  task: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
  tauri::async_runtime::spawn_blocking(task)
    .await
    .map_err(|_| "Host operation was interrupted.".to_string())?
}

#[cfg(test)]
mod network_tests {
  use super::*;
  use crate::settings::{credential_ref, Connection};

  #[test]
  fn offline_generation_resolves_local_demo_and_denies_both_remote_routes() {
    let mut settings = Settings::default();
    let saved = Connection {
      connection_id: "remote".into(),
      name: "Remote fixture".into(),
      provider_id: "custom".into(),
      base_url: "https://example.invalid/v1".into(),
      model_id: "default-model".into(),
      credential: credential_ref("remote"),
    };
    settings.replace(None, saved);
    assert!(matches!(
      resolve_offline_model(ModelRoute::applet(None), &settings).unwrap(),
      OfflineModel::Demo(config) if config.base_url == "demo"
    ));
    let target = LlmTarget {
      connection_id: "remote".into(),
      model_id: "selected-model".into(),
    };
    assert!(matches!(
      resolve_offline_model(ModelRoute::applet(Some(&target)), &settings).unwrap(),
      OfflineModel::Remote(provenance)
        if provenance.connection_id == "remote" && provenance.requested_model_id == "selected-model"
    ));
    settings.active_connection_id = "remote".into();
    assert!(matches!(
      resolve_offline_model(ModelRoute::applet(None), &settings).unwrap(),
      OfflineModel::Remote(provenance)
        if provenance.connection_id == "remote" && provenance.requested_model_id == "default-model"
    ));
  }
}

#[cfg(test)]
mod media_origin_tests {
  use super::require_view_for_media;

  #[test]
  fn persistent_backends_cannot_start_media_or_keep_it_alive_after_view_close() {
    for method in [
      "audio.start",
      "audio.pause",
      "audio.resume",
      "audio.stop",
      "audio.import",
      "audio.share",
      "audio.play",
      "audio.status",
      "audio.capabilities",
      "speech.transcribe",
      "speech.download_model",
      "speech.status",
      "speech.cancel",
    ] {
      let error = require_view_for_media(method, true).unwrap_err();
      assert!(error.contains("attached applet view"));
      assert!(require_view_for_media(method, false).is_ok());
    }
  }

  #[test]
  fn other_backend_services_retain_their_own_origin_policy() {
    for method in [
      "http.request",
      "fs.read",
      "llm.generate",
      "websocket.connect",
      "runtime.info",
    ] {
      assert!(require_view_for_media(method, true).is_ok());
    }
  }
}

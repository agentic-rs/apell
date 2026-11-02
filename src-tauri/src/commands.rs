use crate::host::{public_config, string, Host, Session};
use crate::provider_commands::{snapshot, ModelRoute, ModelSnapshot};
use apell_core::{
  bundles::{digest, sandbox_document, valid_hash, MAX_BUNDLE_BYTES},
  llm::{
    deserialize_target, LlmEvent, LlmFailure, LlmProvenance, LlmRequest, LlmResult, LlmStatus,
    LlmTarget,
  },
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::{
  atomic::{AtomicBool, Ordering},
  Arc, RwLock,
};
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
      let source = tauri::async_runtime::spawn_blocking(move || install_source(&params))
        .await
        .map_err(|_| "Could not prepare applet installation.")??;
      let (bytes, expected_hash) = match source {
        InstallSource::Local {
          bytes,
          expected_hash,
        } => (bytes, expected_hash),
        InstallSource::Url { url, expected_hash } => {
          let app = app.clone();
          let bytes = host
            .resources
            .fetch(
              &url,
              MAX_BUNDLE_BYTES,
              move |received_bytes, total_bytes| {
                let _ = app.emit_to(
                  "main",
                  "install-progress",
                  json!({"received_bytes":received_bytes,"total_bytes":total_bytes}),
                );
              },
            )
            .await?;
          (bytes, expected_hash)
        }
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
            operations: Arc::new(RwLock::new(())),
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
      let selected = snapshot(&app, &host, ModelRoute::applet(request.target.as_ref())).await;
      if !session.alive.load(Ordering::SeqCst) {
        return Err("Applet session is closed.".into());
      }
      let event_session = session_id.clone();
      let emit = move |event: LlmEvent| {
        let _ = app.emit_to(
          "main",
          "applet-event",
          json!({"session_id":event_session,"event":event}),
        );
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

enum InstallSource {
  Local {
    bytes: Vec<u8>,
    expected_hash: String,
  },
  Url {
    url: String,
    expected_hash: String,
  },
}

fn install_source(params: &Value) -> Result<InstallSource, String> {
  let supplied_hash = params
    .get("sha256")
    .map(|value| {
      value
        .as_str()
        .filter(|hash| valid_hash(hash))
        .map(str::to_owned)
        .ok_or_else(|| "Provide a 64-character SHA-256 checksum.".to_string())
    })
    .transpose()?;
  if let Some(bundle) = params.get("bundle") {
    if params.get("url").is_some() {
      return Err("Provide either a local bundle or a bundle URL.".into());
    }
    let bundle = bundle.as_str().ok_or("Invalid local bundle.")?;
    if bundle.len() > MAX_BUNDLE_BYTES {
      return Err("Bundle exceeds the 24 MiB limit.".into());
    }
    return Ok(InstallSource::Local {
      bytes: bundle.as_bytes().to_vec(),
      expected_hash: supplied_hash.unwrap_or_else(|| digest(bundle.as_bytes())),
    });
  }
  Ok(InstallSource::Url {
    url: string(params, "url")?.to_owned(),
    expected_hash: supplied_hash.ok_or("URL installs require a SHA-256 checksum.")?,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use apell_core::bundles::BundleStore;

  fn bundle(version: &str) -> String {
    json!({
      "manifest": {
        "schema_version": 1,
        "api_version": 1,
        "applet_id": "fixture",
        "name": "Fixture",
        "version": version,
        "permissions": []
      },
      "html": "<!doctype html><title>Fixture</title>"
    })
    .to_string()
  }

  fn local_source(params: Value) -> (Vec<u8>, String) {
    match install_source(&params).unwrap() {
      InstallSource::Local {
        bytes,
        expected_hash,
      } => (bytes, expected_hash),
      InstallSource::Url { .. } => panic!("Expected a local bundle."),
    }
  }

  #[test]
  fn local_install_computes_its_checksum_without_browser_crypto() {
    let directory = tempfile::tempdir().unwrap();
    let store = BundleStore::new(directory.path().join("bundles"));
    let text = bundle("1.0.0");
    let (bytes, expected_hash) = local_source(json!({ "bundle": text }));
    assert_eq!(bytes, text.as_bytes());
    assert_eq!(expected_hash, digest(text.as_bytes()));
    let installed = store.install(&bytes, &expected_hash).unwrap();
    assert_eq!(installed.manifest.version, "1.0.0");
    assert_eq!(
      store.load("fixture").unwrap().html,
      "<!doctype html><title>Fixture</title>"
    );
  }

  #[test]
  fn supplied_local_checksum_is_enforced_and_preserves_the_previous_install() {
    let directory = tempfile::tempdir().unwrap();
    let store = BundleStore::new(directory.path().join("bundles"));
    let original = bundle("1.0.0");
    let (bytes, expected_hash) = local_source(json!({
      "bundle": original,
      "sha256": digest(original.as_bytes()).to_uppercase()
    }));
    store.install(&bytes, &expected_hash).unwrap();

    let replacement = bundle("2.0.0");
    let (bytes, expected_hash) = local_source(json!({
      "bundle": replacement,
      "sha256": digest(original.as_bytes())
    }));
    assert!(store.install(&bytes, &expected_hash).is_err());
    assert_eq!(store.load("fixture").unwrap().manifest.version, "1.0.0");
  }

  #[test]
  fn url_installs_require_a_well_formed_supplied_checksum() {
    let url = "https://example.invalid/fixture.apell.json";
    assert!(install_source(&json!({ "url": url })).is_err());
    for sha256 in [Value::Null, json!(12), json!(""), json!("g".repeat(64))] {
      assert!(install_source(&json!({ "url": url, "sha256": sha256 })).is_err());
    }
    let hash = "a".repeat(64);
    match install_source(&json!({ "url": url, "sha256": hash })).unwrap() {
      InstallSource::Url {
        url: actual_url,
        expected_hash,
      } => {
        assert_eq!(actual_url, url);
        assert_eq!(expected_hash, hash);
      }
      InstallSource::Local { .. } => panic!("Expected a URL bundle."),
    }
  }

  #[test]
  fn local_install_rejects_invalid_sources_hashes_and_oversized_bytes() {
    assert!(install_source(&json!({ "bundle": null })).is_err());
    assert!(install_source(&json!({ "bundle": "{}", "sha256": null })).is_err());
    assert!(install_source(&json!({
      "bundle": "{}",
      "url": "https://example.invalid/fixture.apell.json"
    }))
    .is_err());
    let oversized = "x".repeat(MAX_BUNDLE_BYTES + 1);
    assert!(install_source(&json!({ "bundle": oversized })).is_err());
  }
}

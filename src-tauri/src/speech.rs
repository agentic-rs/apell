//! One cancellable speech job across all applets.
mod system;

use crate::host::{string, Host, Session};
use crate::provider_commands::{snapshot, ModelRoute};
use apell_core::llm::LlmTarget;
use serde_json::{json, Value};
use std::sync::{
  atomic::{AtomicBool, AtomicU32, Ordering},
  Arc, Mutex,
};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

#[derive(Default)]
pub struct SpeechJobs {
  active: Mutex<Option<Arc<Job>>>,
}

struct Job {
  id: Uuid,
  owner: String,
  stage: &'static str,
  cancel: Arc<AtomicBool>,
  progress: AtomicU32,
}

struct JobGuard {
  jobs: Arc<SpeechJobs>,
  job: Arc<Job>,
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
  fn drop(&mut self) {
    self.0.store(true, Ordering::SeqCst);
  }
}

impl Drop for JobGuard {
  fn drop(&mut self) {
    self.job.cancel.store(true, Ordering::SeqCst);
    if let Ok(mut active) = self.jobs.active.lock() {
      if active.as_ref().is_some_and(|job| job.id == self.job.id) {
        *active = None;
      }
    }
  }
}

impl SpeechJobs {
  fn begin(self: &Arc<Self>, owner: &str, stage: &'static str) -> Result<JobGuard, String> {
    let mut active = self
      .active
      .lock()
      .map_err(|_| "Speech service unavailable.")?;
    if active.is_some() {
      return Err("A speech job is already running. Wait for it to finish.".into());
    }
    let job = Arc::new(Job {
      id: Uuid::new_v4(),
      owner: owner.into(),
      stage,
      cancel: Arc::new(AtomicBool::new(false)),
      progress: AtomicU32::new(0),
    });
    *active = Some(job.clone());
    Ok(JobGuard {
      jobs: self.clone(),
      job,
    })
  }
}

pub async fn dispatch(
  app: &AppHandle,
  host: &Host,
  owner: &str,
  session: Session,
  method: &str,
  params: Value,
) -> Result<Value, String> {
  if !session.alive.load(Ordering::SeqCst) {
    return Err("Applet session is closed.".into());
  }
  if method == "speech.cancel" {
    let active = host
      .speech_jobs
      .active
      .lock()
      .map_err(|_| "Speech service unavailable.")?;
    if let Some(job) = active.as_ref().filter(|job| job.owner == owner) {
      job.cancel.store(true, Ordering::SeqCst);
    }
    return Ok(Value::Null);
  }
  if method == "speech.status" {
    let status = host.speech.status();
    let system_reason = system::unavailable_reason();
    let active = host
      .speech_jobs
      .active
      .lock()
      .map_err(|_| "Speech service unavailable.")?;
    let own_job = active.as_ref().filter(|job| job.owner == owner);
    return Ok(
      json!({"system_available":system_reason.is_none(), "system_unavailable_reason":system_reason,
      "api_available":api_allowed(&session.manifest), "model_ready":status.ready,"model_size_bytes":status.total_bytes,
      "busy":active.is_some(),"progress":own_job.map(|job|job.progress.load(Ordering::Relaxed)).unwrap_or(0),
      "stage":own_job.map(|job|job.stage).unwrap_or(if active.is_some() {"busy"} else {"idle"})}),
    );
  }
  if !matches!(method, "speech.download_model" | "speech.transcribe") {
    return Err("Unknown speech operation.".into());
  }
  let guard = host.speech_jobs.begin(
    owner,
    if method == "speech.download_model" {
      "downloading"
    } else {
      "transcribing"
    },
  )?;
  let job = guard.job.clone();
  let mut cancellation = session.cancellation.subscribe();
  let watcher_job = job.clone();
  let app = app.clone();
  let method = method.to_owned();
  let _cancel_on_drop = CancelOnDrop(job.cancel.clone());
  // The task, rather than its awaiting bridge request, owns the permit. Dropping
  // the request signals cancellation but cannot admit another worker too early.
  tauri::async_runtime::spawn(async move {
    let watcher = tauri::async_runtime::spawn(async move {
      if !*cancellation.borrow() {
        let _ = cancellation.changed().await;
      }
      watcher_job.cancel.store(true, Ordering::SeqCst);
    });
    let result = run_job(app, session, &method, params, job.clone()).await;
    watcher.abort();
    let cancelled = job.cancel.load(Ordering::SeqCst);
    drop(guard);
    if cancelled {
      Err("Speech operation cancelled.".into())
    } else {
      result
    }
  })
  .await
  .map_err(|_| "Speech job stopped unexpectedly.".to_string())?
}

fn api_allowed(manifest: &apell_core::bundles::Manifest) -> bool {
  manifest.network
    && ["llm", "fs"]
      .iter()
      .all(|permission| manifest.permissions.iter().any(|p| p == *permission))
}

async fn run_job(
  app: AppHandle,
  session: Session,
  method: &str,
  params: Value,
  job: Arc<Job>,
) -> Result<Value, String> {
  let host = app.state::<Host>();
  let speech = host.speech.clone();
  if method == "speech.download_model" {
    let progress = job.clone();
    speech
      .download_model(job.cancel.clone(), move |received, total| {
        let percent = received.saturating_mul(100).checked_div(total).unwrap_or(0);
        progress
          .progress
          .store(percent.min(100) as u32, Ordering::Relaxed);
      })
      .await?;
    return Ok(Value::Null);
  }
  let relative = string(&params, "path")?.to_owned();
  let language = match params.get("language") {
    None | Some(Value::Null) => None,
    Some(Value::String(value))
      if !value.is_empty()
        && value.len() <= 16
        && value
          .bytes()
          .all(|b| b.is_ascii_alphanumeric() || b == b'-') =>
    {
      Some(value.clone())
    }
    _ => return Err("Invalid language selection.".into()),
  };
  let files = host.files.clone();
  let applet_id = session.manifest.applet_id.clone();
  let path =
    crate::commands::blocking(move || files.native_path(&applet_id, &relative, false)).await?;
  if !path.is_file() {
    return Err("Select an audio file to transcribe.".into());
  }
  match params.get("provider") {
    Some(value) if value == "system" => {
      return crate::commands::blocking(move || {
        system::transcribe(&path, language.as_deref(), &job.cancel)
      })
      .await;
    }
    Some(value) if value == "api" => {
      if !api_allowed(&session.manifest) {
        return Err("API transcription requires network, model and file access.".into());
      }
      let target = LlmTarget {
        connection_id: string(&params, "connection_id")?.into(),
        model_id: string(&params, "model_id")?.into(),
      };
      let operation = async {
        let route = ModelRoute::Selected(&target);
        let connection = route.resolve(&*host.settings.lock().await)?;
        if !apell_core::llm::speech_api::supports(&connection.provider_id) {
          return Err("Choose an OpenAI or OpenAI-compatible transcription connection.".into());
        }
        let chosen = snapshot(&app, &host, route).await?;
        let account = chosen.account?;
        apell_core::llm::speech_api::transcribe(
          &path,
          language.as_deref(),
          &chosen.config,
          account.as_ref(),
        )
        .await
      };
      let cancelled = async {
        while !job.cancel.load(Ordering::SeqCst) {
          tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
      };
      return tokio::select! { biased; _ = cancelled => Err("Speech operation cancelled.".into()), result = operation => result };
    }
    None => {}
    Some(value) if value == "downloaded" => {}
    _ => return Err("Unknown speech recognizer.".into()),
  }
  tauri::async_runtime::spawn_blocking(move || {
    let progress = job.clone();
    let transcript = speech.transcribe(
      &path,
      language.as_deref(),
      job.cancel.clone(),
      move |value| {
        progress.progress.store(value.min(100), Ordering::Relaxed);
      },
    )?;
    serde_json::to_value(transcript).map_err(|_| "Could not encode the transcript.".into())
  })
  .await
  .map_err(|_| "The speech worker stopped unexpectedly.".to_string())?
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn remote_transcription_requires_network_files_and_model_access() {
    let mut manifest: apell_core::bundles::Manifest = serde_json::from_value(json!({
      "schema_version":1, "api_version":2, "applet_id":"fixture", "name":"Fixture",
      "version":"1.0.0", "permissions":["speech","llm","fs"], "network":true
    }))
    .unwrap();
    assert!(api_allowed(&manifest));
    manifest.network = false;
    assert!(!api_allowed(&manifest));
    manifest.network = true;
    for permission in ["llm", "fs"] {
      let mut missing = manifest.clone();
      missing.permissions.retain(|value| value != permission);
      assert!(!api_allowed(&missing));
    }
  }

  #[test]
  fn speech_jobs_are_exclusive_and_release_on_failure_or_completion() {
    let jobs = Arc::new(SpeechJobs::default());
    let first = jobs.begin("one", "transcribing").unwrap();
    assert!(jobs.begin("two", "downloading").is_err());
    let cancelled = first.job.cancel.clone();
    drop(first);
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(jobs.begin("two", "downloading").is_ok());
  }
}

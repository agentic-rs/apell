//! One bounded, cancellable on-device speech job across all applets.
use crate::host::{string, Host, Session};
use serde_json::{json, Value};
use std::sync::{
  atomic::{AtomicBool, AtomicU32, Ordering},
  Arc, Mutex,
};
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
    let active = host
      .speech_jobs
      .active
      .lock()
      .map_err(|_| "Speech service unavailable.")?;
    let own_job = active.as_ref().filter(|job| job.owner == owner);
    return Ok(
      json!({"model_ready":status.ready,"model_size_bytes":status.total_bytes,
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
  let files = host.files.clone();
  let speech = host.speech.clone();
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
    let result = run_job(files, speech, session, &method, params, job.clone()).await;
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

async fn run_job(
  files: Arc<apell_core::filesystem::FileStore>,
  speech: Arc<apell_speech::SpeechService>,
  session: Session,
  method: &str,
  params: Value,
  job: Arc<Job>,
) -> Result<Value, String> {
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
    Some(Value::String(value)) if value.len() <= 16 => Some(value.clone()),
    _ => return Err("Invalid language selection.".into()),
  };
  tauri::async_runtime::spawn_blocking(move || {
    let path = files.native_path(&session.manifest.applet_id, &relative, false)?;
    if !path.is_file() {
      return Err("Select an audio file to transcribe.".into());
    }
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

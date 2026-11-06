//! Host-owned, offline speech recognition. No audio is sent over the network.
//! The caller resolves applet-private paths and runs transcription on a blocking worker.
mod chunks;
mod decode;
mod inference;
mod model;

pub use model::{ModelStatus, MODEL_BYTES, MODEL_ID, MODEL_SHA256};
use serde::Serialize;
use std::{
  path::{Path, PathBuf},
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
  },
};

/// Shared file and decoded-duration limits for native audio and recognition.
pub const MAX_AUDIO_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_AUDIO_SECONDS: u64 = 3 * 60 * 60;

/// One shared multilingual model cache. The host gates CPU jobs across applets.
pub struct SpeechService {
  model: model::ModelStore,
}

#[derive(Clone, Debug, Serialize)]
pub struct TranscriptSegment {
  pub start_ms: u64,
  pub end_ms: u64,
  pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Transcript {
  pub text: String,
  pub language: String,
  pub segments: Vec<TranscriptSegment>,
  pub duration_ms: u64,
  pub model_id: String,
}

impl SpeechService {
  pub fn new(root: PathBuf) -> Result<Self, String> {
    whisper_rs::install_logging_hooks();
    Ok(Self {
      model: model::ModelStore::new(root)?,
    })
  }

  pub fn status(&self) -> ModelStatus {
    self.model.status()
  }

  /// Network access is limited to this fixed, integrity-pinned public model.
  pub async fn download_model(
    &self,
    cancel: Arc<AtomicBool>,
    progress: impl Fn(u64, u64) + Send + Sync,
  ) -> Result<ModelStatus, String> {
    self.model.download(cancel, progress).await
  }

  /// Decode, resample to 16 kHz mono, and run Whisper locally on the CPU.
  /// `language` is a Whisper language code, or None for automatic detection.
  pub fn transcribe(
    &self,
    path: &Path,
    language: Option<&str>,
    cancel: Arc<AtomicBool>,
    progress: impl Fn(u32) + Send + 'static,
  ) -> Result<Transcript, String> {
    check_cancelled(&cancel)?;
    let language = inference::normalize_language(language)?;
    let model_path = self.model.verified_path(&cancel)?;
    inference::transcribe(&model_path, path, language.as_deref(), cancel, progress)
  }
}

pub(crate) fn check_cancelled(cancel: &AtomicBool) -> Result<(), String> {
  if cancel.load(Ordering::Acquire) {
    Err("Speech operation cancelled.".into())
  } else {
    Ok(())
  }
}

use crate::check_cancelled;
use reqwest::{redirect::Policy, Client};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
  fs::File,
  io::Read,
  path::{Path, PathBuf},
  sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
  },
  time::Duration,
};
use tokio::io::AsyncWriteExt;

pub const MODEL_ID: &str = "whisper-tiny-multilingual";
pub const MODEL_BYTES: u64 = 77_691_713;
// Publisher LFS pointer: https://huggingface.co/ggerganov/whisper.cpp/blob/main/ggml-tiny.bin
pub const MODEL_SHA256: &str = "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin";

#[derive(Clone, Debug, Serialize)]
pub struct ModelStatus {
  pub model_id: String,
  pub ready: bool,
  pub size_bytes: u64,
  pub total_bytes: u64,
}

pub(crate) struct ModelStore {
  root: PathBuf,
  download_gate: tokio::sync::Mutex<()>,
  verified: AtomicBool,
}

impl ModelStore {
  pub fn new(root: PathBuf) -> Result<Self, String> {
    std::fs::create_dir_all(&root).map_err(|_| "Could not create the speech model directory.")?;
    let verified = verify_file(&root.join("ggml-tiny.bin"), &AtomicBool::new(false)).is_ok();
    Ok(Self {
      root,
      download_gate: tokio::sync::Mutex::new(()),
      verified: AtomicBool::new(verified),
    })
  }

  fn path(&self) -> PathBuf {
    self.root.join("ggml-tiny.bin")
  }

  pub fn status(&self) -> ModelStatus {
    let size_bytes = std::fs::metadata(self.path()).map(|m| m.len()).unwrap_or(0);
    // A model is only moved to this location after its digest is verified.
    // Recheck its digest before every inference, even after a host restart.
    ModelStatus {
      model_id: MODEL_ID.into(),
      ready: size_bytes == MODEL_BYTES && self.verified.load(Ordering::Acquire),
      size_bytes,
      total_bytes: MODEL_BYTES,
    }
  }

  pub fn verified_path(&self, cancel: &AtomicBool) -> Result<PathBuf, String> {
    let path = self.path();
    let result = verify_file(&path, cancel);
    // Cancelling an integrity read does not invalidate an already verified model.
    if !cancel.load(Ordering::Acquire) {
      self.verified.store(result.is_ok(), Ordering::Release);
    }
    result?;
    Ok(path)
  }

  pub async fn download(
    &self,
    cancel: Arc<AtomicBool>,
    progress: impl Fn(u64, u64) + Send + Sync,
  ) -> Result<ModelStatus, String> {
    let _gate = self
      .download_gate
      .try_lock()
      .map_err(|_| "A speech model download is already running.")?;
    check_cancelled(&cancel)?;
    let partial = PartialFile(self.root.join("ggml-tiny.download"));
    self.download_to(&partial.0, &cancel, progress).await?;
    check_cancelled(&cancel)?;
    tokio::fs::rename(&partial.0, self.path())
      .await
      .map_err(|_| "Could not save the speech model.")?;
    self.verified.store(true, Ordering::Release);
    Ok(self.status())
  }

  async fn download_to(
    &self,
    partial: &Path,
    cancel: &AtomicBool,
    progress: impl Fn(u64, u64) + Send + Sync,
  ) -> Result<(), String> {
    let client = model_client()?;
    let mut response = cancellable(cancel, client.get(MODEL_URL).send())
      .await?
      .map_err(|_| {
        "Could not connect to the speech model download. Check your network and retry."
      })?;
    if !response.status().is_success() {
      return Err(format!(
        "Speech model download returned HTTP {}.",
        response.status().as_u16()
      ));
    }
    if response
      .content_length()
      .is_some_and(|length| length != MODEL_BYTES)
    {
      return Err("The speech model download has an unexpected size.".into());
    }
    let mut file = tokio::fs::File::create(partial)
      .await
      .map_err(|_| "Could not create the speech model download.")?;
    let mut received = 0u64;
    let mut hash = Sha256::new();
    progress(0, MODEL_BYTES);
    while let Some(bytes) = cancellable(cancel, response.chunk())
      .await?
      .map_err(|_| "Speech model download was interrupted. Retry to download it again.")?
    {
      check_cancelled(cancel)?;
      received += bytes.len() as u64;
      if received > MODEL_BYTES {
        return Err("The speech model download exceeds its expected size.".into());
      }
      hash.update(&bytes);
      file
        .write_all(&bytes)
        .await
        .map_err(|_| "Could not write the speech model download.")?;
      progress(received, MODEL_BYTES);
    }
    if received != MODEL_BYTES || format!("{:x}", hash.finalize()) != MODEL_SHA256 {
      return Err("Speech model integrity verification failed. Please retry the download.".into());
    }
    file
      .sync_all()
      .await
      .map_err(|_| "Could not finish saving the speech model.")?;
    Ok(())
  }
}

/// Also cleans up when the download future is dropped or an observer panics.
struct PartialFile(PathBuf);

impl Drop for PartialFile {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.0);
  }
}

async fn cancellable<T>(
  cancel: &AtomicBool,
  future: impl std::future::Future<Output = T>,
) -> Result<T, String> {
  tokio::pin!(future);
  loop {
    check_cancelled(cancel)?;
    tokio::select! {
      result = &mut future => return Ok(result),
      _ = tokio::time::sleep(Duration::from_millis(100)) => {}
    }
  }
}

fn model_client() -> Result<Client, String> {
  let builder = Client::builder()
    .https_only(true)
    .connect_timeout(Duration::from_secs(30))
    .timeout(Duration::from_secs(30 * 60))
    .redirect(Policy::custom(|attempt| {
      if attempt.previous().len() >= 5 || attempt.url().scheme() != "https" {
        attempt.error("Invalid speech model redirect.")
      } else {
        attempt.follow()
      }
    }));
  #[cfg(target_os = "android")]
  let builder = {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
      rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "Could not initialize secure model networking.")?
    .with_root_certificates(roots)
    .with_no_client_auth();
    builder.tls_backend_preconfigured(config)
  };
  builder
    .build()
    .map_err(|_| "Could not initialize the speech model download.".into())
}

fn verify_file(path: &Path, cancel: &AtomicBool) -> Result<(), String> {
  let mut file =
    File::open(path).map_err(|_| "Download the offline speech model before transcribing.")?;
  if file
    .metadata()
    .map_err(|_| "Could not inspect the speech model.")?
    .len()
    != MODEL_BYTES
  {
    return Err("The offline speech model is incomplete. Download it again.".into());
  }
  let mut hash = Sha256::new();
  let mut buffer = [0u8; 64 * 1024];
  loop {
    check_cancelled(cancel)?;
    let count = file
      .read(&mut buffer)
      .map_err(|_| "Could not read the offline speech model.")?;
    if count == 0 {
      break;
    }
    hash.update(&buffer[..count]);
  }
  if format!("{:x}", hash.finalize()) != MODEL_SHA256 {
    return Err(
      "The offline speech model failed integrity verification. Download it again.".into(),
    );
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn missing_or_truncated_models_are_not_ready() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path().into()).unwrap();
    assert!(!store.status().ready);
    std::fs::write(store.path(), b"incomplete").unwrap();
    assert!(!store.status().ready);
    assert!(store.verified_path(&AtomicBool::new(false)).is_err());
  }

  #[tokio::test]
  async fn pre_cancelled_download_never_starts_network_or_creates_partial() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path().into()).unwrap();
    assert!(store
      .download(Arc::new(AtomicBool::new(true)), |_, _| panic!(
        "unexpected progress"
      ))
      .await
      .unwrap_err()
      .contains("cancelled"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
  }

  #[tokio::test]
  async fn cancellation_interrupts_a_stalled_network_wait() {
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    tokio::spawn(async move {
      tokio::time::sleep(Duration::from_millis(10)).await;
      flag.store(true, Ordering::Release);
    });
    let result = tokio::time::timeout(
      Duration::from_secs(1),
      cancellable(&cancel, std::future::pending::<()>()),
    )
    .await
    .unwrap();
    assert!(result.unwrap_err().contains("cancelled"));
  }

  #[test]
  fn correctly_sized_corrupt_model_is_rejected_before_inference() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path().into()).unwrap();
    File::create(store.path())
      .unwrap()
      .set_len(MODEL_BYTES)
      .unwrap();
    assert!(!store.status().ready);
    assert!(store
      .verified_path(&AtomicBool::new(false))
      .unwrap_err()
      .contains("integrity"));
  }
}

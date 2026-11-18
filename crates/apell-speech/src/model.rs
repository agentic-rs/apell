use crate::check_cancelled;
use reqwest::{header, redirect::Policy, Client, StatusCode};
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
const MODEL_SOURCES: &[(&str, &str)] = &[
  (
    "Hugging Face",
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
  ),
  (
    "ModelScope",
    "https://modelscope.cn/models/cloudlnk/whisper.cpp/resolve/master/ggml-tiny.bin",
  ),
];

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
    let status = self.status();
    if status.ready {
      return Ok(status);
    }
    let partial = self.root.join("ggml-tiny.download");
    let client = model_client()?;
    let mut failures = Vec::new();
    for &(source, url) in MODEL_SOURCES {
      check_cancelled(&cancel)?;
      match download_from(&client, url, &partial, MODEL_BYTES, &cancel, &progress).await {
        Err(DownloadError::Local(message)) => return Err(message),
        Err(DownloadError::Source(reason)) => {
          failures.push(format!("{source}: {reason}"));
          continue;
        }
        Ok(()) => {}
      }
      let candidate = partial.clone();
      let flag = cancel.clone();
      let verified = tokio::task::spawn_blocking(move || verify_file(&candidate, &flag))
        .await
        .map_err(|_| "Could not verify the speech model.")?;
      check_cancelled(&cancel)?;
      if verified.is_err() {
        // A bad prefix must never be reused by the next source or retry.
        tokio::fs::remove_file(&partial)
          .await
          .map_err(|_| "Could not clear the incomplete speech model.")?;
        failures.push(format!("{source}: model integrity check failed"));
        continue;
      }
      tokio::fs::rename(&partial, self.path())
        .await
        .map_err(|_| "Could not save the speech model.")?;
      self.verified.store(true, Ordering::Release);
      return Ok(self.status());
    }
    Err(format!(
      "Could not download the speech model. {}. Check your connection and retry.",
      failures.join(". ")
    ))
  }
}

#[derive(Debug)]
enum DownloadError {
  // A different source can recover these failures.
  Source(String),
  Local(String),
}

// Keep partial data across network failures and app restarts. Only a correctly
// ranged response may be appended; servers may also return the entire file.
async fn download_from(
  client: &Client,
  url: &str,
  partial: &Path,
  total: u64,
  cancel: &AtomicBool,
  progress: &impl Fn(u64, u64),
) -> Result<(), DownloadError> {
  use DownloadError::{Local, Source};
  let existing = match tokio::fs::metadata(partial).await {
    Ok(metadata) => metadata.len(),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
    Err(_) => return Err(Local("Could not inspect the model download.".into())),
  };
  if existing == total {
    return Ok(());
  }
  let mut offset = if existing < total { existing } else { 0 };
  let (mut response, mut received) = loop {
    let mut request = client
      .get(url)
      .header(header::ACCEPT_ENCODING, "identity")
      .header(header::CACHE_CONTROL, "no-cache");
    if offset > 0 {
      request = request.header(header::RANGE, format!("bytes={offset}-"));
    }
    let response = cancellable(cancel, request.send())
      .await
      .map_err(Local)?
      .map_err(|error| {
        Source(
          if error.is_timeout() {
            "connection timed out"
          } else {
            "could not connect"
          }
          .into(),
        )
      })?;
    match response.status() {
      StatusCode::OK => break (response, 0),
      StatusCode::PARTIAL_CONTENT
        if response
          .headers()
          .get(header::CONTENT_RANGE)
          .and_then(|value| value.to_str().ok())
          == Some(&format!("bytes {offset}-{}/{total}", total - 1)) =>
      {
        break (response, offset)
      }
      // Some caches return an earlier range, or the server may reject Range.
      // Ask once for the whole file before trying the other source.
      StatusCode::PARTIAL_CONTENT | StatusCode::RANGE_NOT_SATISFIABLE if offset > 0 => offset = 0,
      StatusCode::PARTIAL_CONTENT => return Err(Source("unexpected download range".into())),
      status => return Err(Source(format!("HTTP {}", status.as_u16()))),
    }
  };
  if response
    .content_length()
    .is_some_and(|length| length != total - received)
  {
    return Err(Source("unexpected download size".into()));
  }
  let mut file = tokio::fs::OpenOptions::new()
    .create(true)
    .write(true)
    .append(received > 0)
    .truncate(received == 0)
    .open(partial)
    .await
    .map_err(|_| Local("Could not open the model download.".into()))?;
  progress(received, total);
  let transfer = loop {
    let bytes = match cancellable(cancel, response.chunk()).await {
      Ok(Ok(Some(bytes))) => bytes,
      Ok(Ok(None)) if received == total => break Ok(()),
      Ok(Ok(None) | Err(_)) => break Err(Source("download interrupted".into())),
      Err(message) => break Err(Local(message)),
    };
    if bytes.len() as u64 > total - received {
      break Err(Source("download exceeded the expected size".into()));
    }
    file
      .write_all(&bytes)
      .await
      .map_err(|_| Local("Could not save the model download. Check device storage.".into()))?;
    received += bytes.len() as u64;
    progress(received, total);
  };
  // Tokio buffers writes. Settle the last write before a retry measures the prefix.
  file
    .flush()
    .await
    .map_err(|_| Local("Could not save the model download. Check device storage.".into()))?;
  transfer?;
  file
    .sync_all()
    .await
    .map_err(|_| Local("Could not finish saving the speech model.".into()))?;
  Ok(())
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
    .user_agent(concat!("Apell/", env!("CARGO_PKG_VERSION")))
    .https_only(true)
    .no_gzip()
    .no_brotli()
    .no_deflate()
    .no_zstd()
    .read_timeout(Duration::from_secs(45))
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
  use std::{
    io::{BufRead, BufReader, Write},
    net::{Ipv4Addr, TcpListener},
  };

  async fn serve_download(
    path: &Path,
    replies: &'static [(Option<&str>, &[u8])],
  ) -> Result<(), DownloadError> {
    let server = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let url = format!("http://{}/model", server.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
      for &(range, response) in replies {
        let expected = range.map(|value| format!("range: {value}"));
        let (mut stream, _) = server.accept().unwrap();
        let headers: Vec<_> = BufReader::new(&stream)
          .lines()
          .map(Result::unwrap)
          .take_while(|line| !line.is_empty())
          .map(|line| line.to_ascii_lowercase())
          .collect();
        assert_eq!(
          headers.into_iter().find(|line| line.starts_with("range:")),
          expected
        );
        stream.write_all(response).unwrap();
      }
    });
    let client = Client::builder().no_proxy().build().unwrap();
    let result = download_from(&client, &url, path, 6, &AtomicBool::new(false), &|_, _| {}).await;
    worker.join().unwrap();
    result
  }

  #[tokio::test]
  async fn interrupted_download_resumes_from_another_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model.download");
    let interrupted = serve_download(
      &path,
      &[(None, b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nabc")],
    )
    .await;
    assert!(matches!(interrupted, Err(DownloadError::Source(_))));
    assert_eq!(std::fs::read(&path).unwrap(), b"abc");
    serve_download(
      &path,
      &[(Some("bytes=3-"), b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 3-5/6\r\nContent-Length: 3\r\n\r\ndef")],
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
  }

  #[tokio::test]
  async fn invalid_range_retries_the_full_file_without_appending() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model.download");
    std::fs::write(&path, b"old").unwrap();
    serve_download(&path, &[
      (Some("bytes=3-"), b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-5/6\r\nContent-Length: 4\r\nConnection: close\r\n\r\ncdef"),
      (None, b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nabcdef"),
    ]).await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
  }

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

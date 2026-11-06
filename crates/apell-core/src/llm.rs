mod generated;
mod media;
mod platform;
mod provider;
mod sse;
mod tokn_config;
mod types;
mod validation;
mod wire;

pub use types::*;
pub use validation::validate_config;

use provider::Output;
use reqwest::Client;
use std::{
  collections::{HashMap, HashSet},
  sync::{Arc, Mutex},
  time::{Duration, Instant},
};
use tokio::sync::{watch, Semaphore};
use tokn_sdk::account::AccountConfig;
use uuid::Uuid;

const MAX_INPUT_BYTES: usize = 512 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;
const MAX_TOOLS: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

type Failure = LlmFailure;

impl Failure {
  fn new(code: &str, message: &str) -> Self {
    Self {
      code: code.into(),
      message: message.into(),
      capability: None,
      retryable: false,
    }
  }

  fn unsupported(capability: &str) -> Self {
    Self {
      code: "unsupported_capability".into(),
      message: "The provider does not support the requested capability.".into(),
      capability: Some(capability.into()),
      retryable: false,
    }
  }

  fn transport(message: &str) -> Self {
    Self {
      retryable: true,
      ..Self::new("transport_error", message)
    }
  }
}

struct ActiveRequest {
  generation: Uuid,
  cancel: watch::Sender<bool>,
}

type RequestKey = (String, String);

#[derive(Default)]
struct RequestRegistry {
  sessions: HashSet<String>,
  active: HashMap<RequestKey, ActiveRequest>,
  early_cancels: HashMap<RequestKey, Instant>,
}

impl RequestRegistry {
  fn prune(&mut self) {
    self
      .early_cancels
      .retain(|_, received| received.elapsed() < REQUEST_TIMEOUT);
  }
}

type Requests = Arc<Mutex<RequestRegistry>>;

pub struct LlmService {
  client: Client,
  media_client: Client,
  requests: Requests,
  media_jobs: Arc<Semaphore>,
}

/// Applet files supplied by the host only after checking the manifest.
pub struct LlmFiles {
  pub store: Arc<crate::filesystem::FileStore>,
  pub applet_id: String,
}

struct Generation<'a> {
  request: &'a LlmRequest,
  config: &'a LlmConfig,
  account: Option<AccountConfig>,
  files: Option<LlmFiles>,
}

impl LlmService {
  pub fn new() -> Result<Self, String> {
    let client = |read_timeout| {
      crate::http::client_builder()?
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(read_timeout)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not initialize LLM networking.".to_owned())
    };
    Ok(Self {
      client: client(Duration::from_secs(60))?,
      // Buffered image generation can take over a minute before the first byte.
      media_client: client(REQUEST_TIMEOUT)?,
      requests: Arc::new(Mutex::new(RequestRegistry::default())),
      media_jobs: Arc::new(Semaphore::new(2)),
    })
  }

  pub fn register_session(&self, session_id: &str) -> Result<(), String> {
    let mut requests = self
      .requests
      .lock()
      .map_err(|_| "LLM request state is unavailable.")?;
    if !validation::valid_id(session_id) || requests.sessions.contains(session_id) {
      return Err("The LLM session is already registered or invalid.".into());
    }
    if requests.sessions.len() >= 256 {
      return Err("Too many applet sessions are active.".into());
    }
    requests.sessions.insert(session_id.into());
    Ok(())
  }

  /// The result is the sole terminal outcome; events report progress only.
  /// Conversation messages and provider metadata are owned by the applet.
  pub async fn generate(
    &self,
    session_id: &str,
    request: LlmRequest,
    config: LlmConfig,
    account: Option<AccountConfig>,
    emit: impl Fn(LlmEvent) + Send + Sync + 'static,
  ) -> LlmResult {
    self
      .generate_with_files(session_id, request, config, account, None, emit)
      .await
  }

  pub async fn generate_with_files(
    &self,
    session_id: &str,
    request: LlmRequest,
    config: LlmConfig,
    account: Option<AccountConfig>,
    files: Option<LlmFiles>,
    emit: impl Fn(LlmEvent) + Send + Sync + 'static,
  ) -> LlmResult {
    let provenance = request
      .target
      .as_ref()
      .map_or_else(|| config.provenance(), LlmTarget::provenance);
    let mut output = Output::new(&request.request_id, provenance);
    let result = self
      .run(
        session_id,
        Generation {
          request: &request,
          config: &config,
          account,
          files,
        },
        &mut output,
        &emit,
      )
      .await;
    output.result(result)
  }

  async fn run(
    &self,
    session_id: &str,
    generation: Generation<'_>,
    output: &mut Output,
    emit: &impl Fn(LlmEvent),
  ) -> Result<LlmStatus, Failure> {
    let Generation {
      request,
      config,
      account,
      files,
    } = generation;
    if let Some(target) = &request.target {
      target
        .validate_for(config)
        .map_err(|message| Failure::new("invalid_target", &message))?;
    } else {
      validate_config(config).map_err(|_| {
        Failure::new(
          "invalid_configuration",
          "The LLM connection configuration is invalid.",
        )
      })?;
    }
    validation::request(request)?;
    tokn_config::validate_account(config, account.as_ref())?;
    let (mut cancellation, guard) = self.register(session_id, &request.request_id)?;
    let protocol = (config.base_url != "demo")
      .then(|| wire::protocol(config))
      .transpose()?;
    if let Some(output) = &request.output {
      if !protocol.is_some_and(|protocol| protocol.supports_output(config, output)) {
        return Err(Failure::unsupported(output.capability()));
      }
      if files.is_none() {
        return Err(Failure::new(
          "media_output_permission",
          "The applet must declare file access to receive generated media.",
        ));
      }
    }
    let mut file_kinds = request
      .messages
      .iter()
      .filter_map(media::parts)
      .flatten()
      .filter_map(|part| part["type"].as_str())
      .filter(|kind| *kind != "text");
    let first_file = file_kinds.next();
    if protocol.is_none()
      && (!request.tools.is_empty()
        || request.messages.iter().any(|message| {
          message
            .get("tool_calls")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|calls| !calls.is_empty())
            || message["role"] == "tool"
        }))
    {
      return Err(Failure::unsupported("tools"));
    }
    if let Some(kind) = first_file {
      let Some(protocol) = protocol else {
        return Err(Failure::unsupported(match kind {
          "image" => "image_input",
          "document" => "pdf_input",
          _ => "audio_input",
        }));
      };
      if !protocol.supports_audio_input()
        && (kind == "audio" || file_kinds.any(|kind| kind == "audio"))
      {
        return Err(Failure::unsupported("audio_input"));
      }
    }
    emit(LlmEvent {
      provenance: Some(output.provenance.clone()),
      ..event(&request.request_id, "started", "")
    });
    let mut permit = if first_file.is_some() || request.output.is_some() {
      Some(
        self
          .media_jobs
          .clone()
          .try_acquire_owned()
          .map_err(|_| Failure {
            retryable: true,
            ..Failure::new("busy", "Too many LLM requests using media are active.")
          })?,
      )
    } else {
      None
    };
    // The accumulator outlives the network future, so cancellation and transport
    // failures return the exact partial answer regardless of event delivery order.
    let status = {
      let work = async {
        if protocol.is_none() {
          return provider::demo(request, output, emit).await;
        }
        let attachments = if first_file.is_some() {
          let worker_permit = permit.take();
          let access = files
            .as_ref()
            .map(|files| (files.store.clone(), files.applet_id.clone()));
          let messages = request.messages.clone();
          // The blocking worker retains the permit even if the awaiting
          // request is canceled before the bounded file read has finished.
          let (attachments, worker_permit) = tokio::task::spawn_blocking(move || {
            let access = access.as_ref().map(|(store, applet_id)| media::FileAccess {
              files: store,
              applet_id,
            });
            media::resolve(&messages, access).map(|attachments| (attachments, worker_permit))
          })
          .await
          .map_err(|_| {
            Failure::new("internal_error", "The attachment worker could not finish.")
          })??;
          permit = worker_permit;
          attachments
        } else {
          media::Attachments::new()
        };
        let client = if request.output.is_some() {
          &self.media_client
        } else {
          &self.client
        };
        provider::remote(client, request, config, account, attachments, output, emit).await
      };
      tokio::select! {
        biased;
        _ = canceled(&mut cancellation) => Err(Failure::new("cancelled", "LLM request canceled.")),
        result = tokio::time::timeout(REQUEST_TIMEOUT, work) => result.unwrap_or_else(|_| Err(Failure::transport("The LLM request timed out."))),
      }
    }?;
    if matches!(status, LlmStatus::Completed | LlmStatus::ToolCalls) {
      if !output.generated.is_empty() {
        let files = files
          .ok_or_else(|| Failure::new("internal_error", "No applet file store is available."))?;
        output.media = publish_generated(
          files,
          std::mem::take(&mut output.generated),
          self.requests.clone(),
          guard.key.clone(),
          guard.generation,
          permit,
        )
        .await?;
        return Ok(status);
      }
      let registry = self
        .requests
        .lock()
        .map_err(|_| Failure::new("internal_error", "LLM request state is unavailable."))?;
      // A cancellation accepted before completion must remain authoritative,
      // even if the provider finished in the same poll.
      if !registry.sessions.contains(session_id)
        || !registry
          .active
          .get(&guard.key)
          .is_some_and(|active| active.generation == guard.generation && !*active.cancel.borrow())
      {
        return Err(Failure::new("cancelled", "LLM request canceled."));
      }
    }
    Ok(status)
  }

  pub fn cancel(&self, session_id: &str, request_id: &str) -> Result<(), String> {
    if !validation::valid_id(request_id) {
      return Err("The LLM request ID is invalid.".into());
    }
    let mut requests = self
      .requests
      .lock()
      .map_err(|_| "LLM request state is unavailable.")?;
    if !requests.sessions.contains(session_id) {
      return Err("The applet session has closed or is not registered.".into());
    }
    requests.prune();
    let key = (session_id.into(), request_id.into());
    if let Some(request) = requests.active.get(&key) {
      request.cancel.send_replace(true);
    } else {
      if requests.early_cancels.len() >= 256 {
        return Err("Too many pending LLM cancellations.".into());
      }
      requests.early_cancels.insert(key, Instant::now());
    }
    Ok(())
  }

  pub fn close_session(&self, session_id: &str) {
    let mut requests = self
      .requests
      .lock()
      .unwrap_or_else(|error| error.into_inner());
    requests.sessions.remove(session_id);
    requests.active.retain(|(owner, _), request| {
      if owner == session_id {
        request.cancel.send_replace(true);
        false
      } else {
        true
      }
    });
    requests
      .early_cancels
      .retain(|(owner, _), _| owner != session_id);
  }

  fn register(
    &self,
    session_id: &str,
    request_id: &str,
  ) -> Result<(watch::Receiver<bool>, RequestGuard), Failure> {
    let key = (session_id.to_owned(), request_id.to_owned());
    let mut requests = self
      .requests
      .lock()
      .map_err(|_| Failure::new("internal_error", "LLM request state is unavailable."))?;
    if !requests.sessions.contains(session_id) {
      return Err(Failure::new(
        "session_closed",
        "The applet session has closed or is not registered.",
      ));
    }
    requests.prune();
    if requests.early_cancels.remove(&key).is_some() {
      return Err(Failure::new("cancelled", "LLM request canceled."));
    }
    if requests.active.contains_key(&key) {
      return Err(Failure::new(
        "duplicate_request",
        "This LLM request is already active.",
      ));
    }
    if requests.active.len() >= 16
      || requests
        .active
        .keys()
        .filter(|(owner, _)| owner == session_id)
        .count()
        >= 4
    {
      return Err(Failure {
        retryable: true,
        ..Failure::new(
          "busy",
          "Too many LLM requests are active. Stop a request before trying again.",
        )
      });
    }
    let generation = Uuid::new_v4();
    let (cancel, receiver) = watch::channel(false);
    requests
      .active
      .insert(key.clone(), ActiveRequest { generation, cancel });
    Ok((
      receiver,
      RequestGuard {
        requests: self.requests.clone(),
        key,
        generation,
      },
    ))
  }
}

async fn publish_generated(
  files: LlmFiles,
  assets: Vec<generated::Asset>,
  requests: Requests,
  key: RequestKey,
  generation: Uuid,
  permit: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Result<Vec<LlmMediaFile>, Failure> {
  tokio::task::spawn_blocking(move || {
    let _permit = permit;
    let names: Vec<_> = assets
      .iter()
      .enumerate()
      .map(|(index, asset)| format!("{}.{}", index + 1, asset.extension))
      .collect();
    let file_error = |_| {
      Failure::new(
        "media_output_storage",
        "Could not save the generated media.",
      )
    };
    let batch = files.store.stage_generated(
      names
        .iter()
        .zip(&assets)
        .map(|(name, asset)| (name.as_str(), &asset.bytes[..])),
    );
    // Commit and cancellation share the same lock. A canceled generation
    // drops its staging directory without ever publishing part of the batch.
    let mut requests = requests
      .lock()
      .map_err(|_| Failure::new("internal_error", "LLM request state is unavailable."))?;
    if !requests.sessions.contains(&key.0)
      || !requests
        .active
        .get(&key)
        .is_some_and(|active| active.generation == generation && !*active.cancel.borrow())
    {
      return Err(Failure::new("cancelled", "LLM request canceled."));
    }
    let directory = files
      .store
      .publish_generated(&files.applet_id, batch.map_err(file_error)?)
      .map_err(file_error)?;
    requests.active.remove(&key);
    drop(requests);
    Ok(
      names
        .into_iter()
        .zip(assets)
        .map(|(name, asset)| LlmMediaFile {
          kind: if asset.mime_type.starts_with("image/") {
            LlmMediaKind::Image
          } else {
            LlmMediaKind::Audio
          },
          path: format!("{directory}/{name}"),
          mime_type: asset.mime_type.into(),
          size_bytes: asset.bytes.len(),
          transcript: asset.transcript,
        })
        .collect(),
    )
  })
  .await
  .map_err(|_| Failure::new("internal_error", "The media worker could not finish."))?
}

fn event(request_id: &str, kind: &str, text: &str) -> LlmEvent {
  LlmEvent {
    request_id: request_id.into(),
    kind: kind.into(),
    text: text.into(),
    provenance: None,
  }
}

async fn canceled(receiver: &mut watch::Receiver<bool>) {
  loop {
    let is_canceled = *receiver.borrow_and_update();
    if is_canceled || receiver.changed().await.is_err() {
      return;
    }
  }
}

struct RequestGuard {
  requests: Requests,
  key: RequestKey,
  generation: Uuid,
}

impl Drop for RequestGuard {
  fn drop(&mut self) {
    let mut requests = self
      .requests
      .lock()
      .unwrap_or_else(|error| error.into_inner());
    if requests
      .active
      .get(&self.key)
      .is_some_and(|request| request.generation == self.generation)
    {
      requests.active.remove(&self.key);
    }
  }
}

#[cfg(test)]
mod tests;

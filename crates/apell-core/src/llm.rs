use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
  collections::{HashMap, HashSet},
  sync::{Arc, Mutex},
  time::{Duration, Instant},
};
use tokio::sync::watch;
use uuid::Uuid;

const MAX_INPUT_BYTES: usize = 512 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
  pub base_url: String,
  pub model_id: String,
}

impl Default for LlmConfig {
  fn default() -> Self {
    Self {
      base_url: "demo".into(),
      model_id: "local-demo".into(),
    }
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmMessage {
  pub role: String,
  pub content: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmRequest {
  pub request_id: String,
  pub messages: Vec<LlmMessage>,
  #[serde(default)]
  pub temperature: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LlmEvent {
  pub request_id: String,
  pub kind: String,
  pub text: String,
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
  requests: Requests,
}

impl LlmService {
  pub fn new() -> Result<Self, String> {
    let client = crate::http::client_builder()?
      .connect_timeout(Duration::from_secs(20))
      .read_timeout(Duration::from_secs(60))
      .timeout(REQUEST_TIMEOUT)
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .map_err(|_| "Could not initialize LLM networking.".to_owned())?;
    Ok(Self {
      client,
      requests: Arc::new(Mutex::new(RequestRegistry::default())),
    })
  }

  /// Register a host-created session before exposing its applet bridge.
  pub fn register_session(&self, session_id: &str) -> Result<(), String> {
    let mut requests = self
      .requests
      .lock()
      .map_err(|_| "LLM request state is unavailable.")?;
    if !valid_request_id(session_id) || requests.sessions.contains(session_id) {
      return Err("The LLM session is already registered or invalid.".into());
    }
    if requests.sessions.len() >= 256 {
      return Err("Too many applet sessions are active.".into());
    }
    requests.sessions.insert(session_id.into());
    Ok(())
  }

  pub async fn complete(
    &self,
    session_id: &str,
    request: LlmRequest,
    config: LlmConfig,
    api_key: Option<String>,
    emit: impl Fn(LlmEvent) + Send + Sync + 'static,
  ) -> Result<Value, String> {
    validate_config(&config)?;
    validate_request(&request)?;
    if let Some(key) = &api_key {
      if key.len() > 8192 || key.chars().any(char::is_control) {
        return Err("The provider credential is invalid.".into());
      }
    }
    let (mut cancellation, _guard) = self.register(session_id, &request.request_id)?;
    let work = async {
      if config.base_url == "demo" {
        self.demo(&request, &emit).await
      } else {
        self.remote(&request, &config, api_key, &emit).await
      }
    };
    let content = tokio::select! {
      biased;
      _ = canceled(&mut cancellation) => return Err("LLM request canceled.".into()),
      result = tokio::time::timeout(REQUEST_TIMEOUT, work) => {
        result.map_err(|_| "The LLM request timed out.".to_owned())??
      }
    };
    emit(LlmEvent {
      request_id: request.request_id.clone(),
      kind: "done".into(),
      text: String::new(),
    });
    Ok(json!({ "request_id": request.request_id, "content": content }))
  }

  pub fn cancel(&self, session_id: &str, request_id: &str) -> Result<(), String> {
    if !valid_request_id(request_id) {
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
      // Native commands can overtake each other while configuration or secure
      // storage is being read. Preserve Stop until the matching request starts.
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
  ) -> Result<(watch::Receiver<bool>, RequestGuard), String> {
    let key = (session_id.to_owned(), request_id.to_owned());
    let mut requests = self
      .requests
      .lock()
      .map_err(|_| "LLM request state is unavailable.")?;
    if !requests.sessions.contains(session_id) {
      return Err("The applet session has closed or is not registered.".into());
    }
    requests.prune();
    if requests.early_cancels.remove(&key).is_some() {
      return Err("LLM request canceled.".into());
    }
    if requests.active.contains_key(&key) {
      return Err("This LLM request is already active.".into());
    }
    if requests.active.len() >= 16
      || requests
        .active
        .keys()
        .filter(|(owner, _)| owner == session_id)
        .count()
        >= 4
    {
      return Err("Too many LLM requests are active. Stop a request before trying again.".into());
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

  async fn demo(&self, request: &LlmRequest, emit: &impl Fn(LlmEvent)) -> Result<String, String> {
    let prompt = request
      .messages
      .iter()
      .rev()
      .find(|message| message.role == "user")
      .map(|message| message.content.chars().take(200).collect::<String>())
      .unwrap_or_default();
    let answer = format!(
      "This is the local demo provider. Your message was: “{prompt}”\n\nThe applet is running Rust WebAssembly, and this conversation is stored by the shell’s native SQLite service. Configure a provider in the shell settings for real model responses."
    );
    for text in answer.split_inclusive(' ') {
      tokio::time::sleep(Duration::from_millis(18)).await;
      emit(delta(&request.request_id, text));
    }
    Ok(answer)
  }

  async fn remote(
    &self,
    request: &LlmRequest,
    config: &LlmConfig,
    api_key: Option<String>,
    emit: &impl Fn(LlmEvent),
  ) -> Result<String, String> {
    let mut body = json!({
      "model": config.model_id,
      "messages": request.messages,
      "stream": true,
    });
    if let Some(temperature) = request.temperature {
      body["temperature"] = json!(temperature);
    }
    let mut builder = self.client.post(completion_url(config)?).json(&body);
    if let Some(key) = api_key.filter(|key| !key.is_empty()) {
      builder = builder.bearer_auth(key);
    }
    let mut response = builder
      .send()
      .await
      .map_err(|_| "Could not connect to the LLM provider.".to_owned())?;
    if !response.status().is_success() {
      // Provider bodies and transport errors may contain credentials or private endpoints.
      return Err(format!(
        "The LLM provider returned HTTP {}.",
        response.status().as_u16()
      ));
    }
    let content_type = response
      .headers()
      .get(reqwest::header::CONTENT_TYPE)
      .and_then(|value| value.to_str().ok())
      .unwrap_or("")
      .to_ascii_lowercase();
    if content_type.starts_with("application/json") {
      let mut bytes = Vec::new();
      while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "The LLM response was interrupted.".to_owned())?
      {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
          return Err("The LLM response exceeded the size limit.".into());
        }
        bytes.extend_from_slice(&chunk);
      }
      let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "The LLM provider returned invalid JSON.".to_owned())?;
      let text = buffered_text(&value)?;
      emit(delta(&request.request_id, &text));
      return Ok(text);
    }
    if !content_type.starts_with("text/event-stream") {
      return Err("The LLM provider did not return a supported response format.".into());
    }
    let mut parser = SseParser::default();
    let mut output = Completion::default();
    let mut received = 0usize;
    while let Some(chunk) = response
      .chunk()
      .await
      .map_err(|_| "The LLM response was interrupted.".to_owned())?
    {
      received = received.saturating_add(chunk.len());
      if received > MAX_RESPONSE_BYTES {
        return Err("The LLM response exceeded the size limit.".into());
      }
      for data in parser.push(&chunk)? {
        let text = output.consume(&data)?;
        if !text.is_empty() {
          emit(delta(&request.request_id, &text));
        }
        if output.done {
          return output.finish();
        }
      }
    }
    // Incomplete SSE records are intentionally discarded. A transport ending
    // without a completion marker must not turn a partial answer into success.
    output.finish()
  }
}

fn delta(request_id: &str, text: &str) -> LlmEvent {
  LlmEvent {
    request_id: request_id.into(),
    kind: "delta".into(),
    text: text.into(),
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

pub fn validate_config(config: &LlmConfig) -> Result<(), String> {
  if config.model_id.trim().is_empty()
    || config.model_id.len() > 256
    || config.model_id.chars().any(char::is_control)
  {
    return Err("Enter a valid model ID.".into());
  }
  if config.base_url != "demo" {
    completion_url(config)?;
  }
  Ok(())
}

fn completion_url(config: &LlmConfig) -> Result<Url, String> {
  if config.base_url.len() > 2048 {
    return Err("The provider URL is too long.".into());
  }
  let mut url =
    Url::parse(&config.base_url).map_err(|_| "Enter a valid provider URL.".to_owned())?;
  let host = url.host_str().unwrap_or("");
  let loopback = host == "localhost"
    || host
      .trim_matches(['[', ']'])
      .parse::<std::net::IpAddr>()
      .is_ok_and(|address| address.is_loopback());
  if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
    return Err(
      "Provider URLs must use HTTPS, except HTTP on loopback for local development.".into(),
    );
  }
  if host.is_empty()
    || !url.username().is_empty()
    || url.password().is_some()
    || url.query().is_some()
    || url.fragment().is_some()
  {
    return Err(
      "Provider URLs must not include credentials, query parameters, or fragments.".into(),
    );
  }
  let path = format!("{}/chat/completions", url.path().trim_end_matches('/'));
  url.set_path(&path);
  Ok(url)
}

fn validate_request(request: &LlmRequest) -> Result<(), String> {
  if !valid_request_id(&request.request_id) {
    return Err("The LLM request ID is invalid.".into());
  }
  if request.messages.is_empty() || request.messages.len() > 256 {
    return Err("An LLM request must contain between 1 and 256 messages.".into());
  }
  let mut size = 0usize;
  for message in &request.messages {
    if !matches!(message.role.as_str(), "system" | "user" | "assistant") {
      return Err("The LLM message role is unsupported.".into());
    }
    size = size.saturating_add(message.content.len());
  }
  if size == 0 || size > MAX_INPUT_BYTES {
    return Err("The LLM request is empty or exceeds the context size limit.".into());
  }
  if request
    .temperature
    .is_some_and(|value| !value.is_finite() || !(0.0..=2.0).contains(&value))
  {
    return Err("Temperature must be between 0 and 2.".into());
  }
  Ok(())
}

fn valid_request_id(request_id: &str) -> bool {
  !request_id.is_empty()
    && request_id.len() <= 128
    && request_id
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn buffered_text(value: &Value) -> Result<String, String> {
  if value.get("error").is_some() {
    return Err("The LLM provider rejected the request.".into());
  }
  let message = &value["choices"][0]["message"];
  if message
    .get("tool_calls")
    .is_some_and(|value| !value.is_null())
  {
    return Err("This text completion API does not support tool calls.".into());
  }
  let text = message["content"]
    .as_str()
    .ok_or("The LLM provider returned no text.")?;
  if text.is_empty() || text.len() > MAX_OUTPUT_BYTES {
    return Err("The LLM response is empty or exceeds the size limit.".into());
  }
  Ok(text.into())
}

#[derive(Default)]
struct Completion {
  content: String,
  done: bool,
  finished: bool,
}

impl Completion {
  fn consume(&mut self, data: &str) -> Result<String, String> {
    if data.trim() == "[DONE]" {
      self.done = true;
      return Ok(String::new());
    }
    let value: Value = serde_json::from_str(data)
      .map_err(|_| "The LLM provider returned an invalid stream event.".to_owned())?;
    if value.get("error").is_some() {
      return Err("The LLM provider rejected the request.".into());
    }
    let Some(choice) = value
      .get("choices")
      .and_then(Value::as_array)
      .and_then(|choices| choices.first())
    else {
      // Some providers send usage-only or keepalive JSON events.
      return Ok(String::new());
    };
    let delta = &choice["delta"];
    if delta
      .get("tool_calls")
      .is_some_and(|value| !value.is_null())
    {
      return Err("This text completion API does not support tool calls.".into());
    }
    let text = match delta.get("content") {
      Some(Value::String(text)) => text.as_str(),
      None | Some(Value::Null) => "",
      _ => return Err("The LLM provider returned an unsupported text delta.".into()),
    };
    if self.content.len().saturating_add(text.len()) > MAX_OUTPUT_BYTES {
      return Err("The LLM response exceeded the size limit.".into());
    }
    self.content.push_str(text);
    self.finished |= choice
      .get("finish_reason")
      .is_some_and(|value| !value.is_null());
    Ok(text.into())
  }

  fn finish(self) -> Result<String, String> {
    if !self.done && !self.finished {
      return Err("The LLM stream ended before the answer was complete.".into());
    }
    if self.content.is_empty() {
      return Err("The LLM provider returned no text.".into());
    }
    Ok(self.content)
  }
}

#[derive(Default)]
struct SseParser {
  line: Vec<u8>,
  data: Vec<String>,
  data_bytes: usize,
  previous_cr: bool,
}

impl SseParser {
  fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, String> {
    let mut events = Vec::new();
    for &byte in bytes {
      if self.previous_cr {
        self.previous_cr = false;
        if byte == b'\n' {
          continue;
        }
      }
      if matches!(byte, b'\r' | b'\n') {
        self.process_line(&mut events)?;
        self.previous_cr = byte == b'\r';
      } else {
        if self.line.len() >= MAX_OUTPUT_BYTES {
          return Err("The LLM stream event exceeded the size limit.".into());
        }
        self.line.push(byte);
      }
    }
    Ok(events)
  }

  fn process_line(&mut self, events: &mut Vec<String>) -> Result<(), String> {
    if self.line.is_empty() {
      if !self.data.is_empty() {
        events.push(self.data.join("\n"));
        self.data.clear();
        self.data_bytes = 0;
      }
      return Ok(());
    }
    let line = std::str::from_utf8(&self.line)
      .map_err(|_| "The LLM stream contains invalid UTF-8.".to_owned())?;
    let (field, value) = line.split_once(':').unwrap_or((line, ""));
    if field == "data" {
      let value = value.strip_prefix(' ').unwrap_or(value);
      self.data_bytes = self.data_bytes.saturating_add(value.len() + 1);
      if self.data_bytes > MAX_OUTPUT_BYTES {
        return Err("The LLM stream event exceeded the size limit.".into());
      }
      self.data.push(value.into());
    }
    self.line.clear();
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn request(id: &str) -> LlmRequest {
    LlmRequest {
      request_id: id.into(),
      messages: vec![LlmMessage {
        role: "user".into(),
        content: "Hello".into(),
      }],
      temperature: None,
    }
  }

  #[test]
  fn sse_preserves_unicode_at_every_chunk_boundary() {
    let input =
      "data: {\"choices\":[{\"delta\":{\"content\":\"你好 🌍\"}}]}\r\n\r\ndata: [DONE]\r\n\r\n";
    for boundary in 0..=input.len() {
      let mut parser = SseParser::default();
      let mut events = parser.push(&input.as_bytes()[..boundary]).unwrap();
      events.extend(parser.push(&input.as_bytes()[boundary..]).unwrap());
      let mut completion = Completion::default();
      for event in events {
        completion.consume(&event).unwrap();
      }
      assert_eq!(completion.finish().unwrap(), "你好 🌍");
    }
  }

  #[test]
  fn sse_handles_cr_comments_and_multiline_data() {
    let mut parser = SseParser::default();
    let events = parser
      .push(b":heartbeat\revent: message\rdata: first\rdata:second\r\r")
      .unwrap();
    assert_eq!(events, vec!["first\nsecond"]);
    assert!(parser.push(b"data: unfinished").unwrap().is_empty());
    assert_eq!(parser.push(b"\n\n").unwrap(), vec!["unfinished"]);
  }

  #[test]
  fn rejects_unfinished_and_oversized_streams_without_leaking_provider_errors() {
    let mut completion = Completion::default();
    completion
      .consume(r#"{"choices":[{"delta":{"content":"partial"}}]}"#)
      .unwrap();
    assert!(completion.finish().unwrap_err().contains("before"));
    let error = Completion::default()
      .consume(r#"{"error":{"message":"private-key-material"}}"#)
      .unwrap_err();
    assert!(!error.contains("private-key-material"));
    assert!(SseParser::default()
      .push(&vec![b'x'; MAX_OUTPUT_BYTES + 1])
      .is_err());
  }

  #[test]
  fn validates_provider_urls_and_request_limits() {
    assert!(validate_config(&LlmConfig::default()).is_ok());
    for base_url in [
      "file:///tmp/test",
      "http://provider.invalid/v1",
      "https://user:secret@provider.invalid/v1",
      "https://provider.invalid/v1?key=secret",
      "https://provider.invalid/v1#fragment",
    ] {
      assert!(validate_config(&LlmConfig {
        base_url: base_url.into(),
        model_id: "model".into()
      })
      .is_err());
    }
    for base_url in [
      "https://provider.invalid/v1/",
      "http://127.0.0.1:8000/v1",
      "http://[::1]:8000/v1",
    ] {
      let config = LlmConfig {
        base_url: base_url.into(),
        model_id: "model".into(),
      };
      assert!(validate_config(&config).is_ok());
      assert_eq!(
        completion_url(&config).unwrap().path(),
        "/v1/chat/completions"
      );
    }
    let mut invalid = request("id");
    invalid.temperature = Some(f64::NAN);
    assert!(validate_request(&invalid).is_err());
    invalid.temperature = None;
    invalid.messages[0].content = "x".repeat(MAX_INPUT_BYTES + 1);
    assert!(validate_request(&invalid).is_err());
  }

  #[tokio::test]
  async fn cancellation_is_scoped_and_releases_registry() {
    let service = Arc::new(LlmService::new().unwrap());
    service.register_session("one").unwrap();
    service.register_session("two").unwrap();
    let (mut receiver, guard) = service.register("one", "same-id").unwrap();
    assert!(service.register("one", "same-id").is_err());
    service.cancel("two", "same-id").unwrap();
    assert!(!*receiver.borrow());
    service.cancel("one", "same-id").unwrap();
    canceled(&mut receiver).await;
    drop(guard);
    assert!(service.requests.lock().unwrap().active.is_empty());

    let task_service = service.clone();
    let task = tokio::spawn(async move {
      task_service
        .complete(
          "one",
          request("running"),
          LlmConfig::default(),
          None,
          |_| {},
        )
        .await
    });
    for _ in 0..100 {
      if !service.requests.lock().unwrap().active.is_empty() {
        break;
      }
      tokio::task::yield_now().await;
    }
    service.close_session("one");
    assert_eq!(task.await.unwrap().unwrap_err(), "LLM request canceled.");
    assert!(service.requests.lock().unwrap().active.is_empty());
  }

  #[test]
  fn stale_guard_cannot_remove_replacement_request() {
    let service = LlmService::new().unwrap();
    service.register_session("session").unwrap();
    let (_, old) = service.register("session", "request").unwrap();
    service.close_session("session");
    service.register_session("session").unwrap();
    let (_, current) = service.register("session", "request").unwrap();
    drop(old);
    assert_eq!(service.requests.lock().unwrap().active.len(), 1);
    drop(current);
    assert!(service.requests.lock().unwrap().active.is_empty());
  }

  #[tokio::test]
  async fn early_cancel_prevents_request_start_only_in_its_session() {
    let service = LlmService::new().unwrap();
    service.register_session("session").unwrap();
    service.register_session("another").unwrap();
    service.cancel("session", "request").unwrap();
    let result = service
      .complete(
        "session",
        request("request"),
        LlmConfig::default(),
        None,
        |_| {
          panic!("A canceled request must emit nothing");
        },
      )
      .await;
    assert_eq!(result.unwrap_err(), "LLM request canceled.");
    assert!(service.register("another", "request").is_ok());
  }

  #[tokio::test]
  async fn demo_completion_stream_matches_return_value() {
    let service = LlmService::new().unwrap();
    service.register_session("session").unwrap();
    let events = Arc::new(Mutex::new(Vec::<LlmEvent>::new()));
    let sink = events.clone();
    let result = service
      .complete(
        "session",
        request("demo"),
        LlmConfig::default(),
        None,
        move |event| {
          sink.lock().unwrap().push(event);
        },
      )
      .await
      .unwrap();
    let events = events.lock().unwrap();
    assert_eq!(events.last().unwrap().kind, "done");
    assert!(events.iter().all(|event| event.request_id == "demo"));
    let streamed = events
      .iter()
      .filter(|event| event.kind == "delta")
      .map(|event| event.text.as_str())
      .collect::<String>();
    assert_eq!(result["content"].as_str().unwrap(), streamed);
    assert!(streamed.starts_with("This is the local demo provider."));
    assert!(service.requests.lock().unwrap().active.is_empty());
  }

  #[tokio::test]
  async fn closed_or_unregistered_session_cannot_start_work() {
    let service = LlmService::new().unwrap();
    assert!(service.register("missing", "request").is_err());
    assert!(service.cancel("missing", "request").is_err());
    service.register_session("session").unwrap();
    assert!(service.register_session("session").is_err());
    service.cancel("session", "early").unwrap();
    service.close_session("session");
    let result = service
      .complete(
        "session",
        request("late"),
        LlmConfig::default(),
        None,
        |_| {
          panic!("A closed session must emit nothing");
        },
      )
      .await;
    assert_eq!(
      result.unwrap_err(),
      "The applet session has closed or is not registered."
    );
    assert!(service.cancel("session", "late").is_err());
    let registry = service.requests.lock().unwrap();
    assert!(registry.active.is_empty());
    assert!(registry.early_cancels.is_empty());
    assert!(registry.sessions.is_empty());
  }
}

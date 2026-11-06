use super::{
  event, generated,
  media::{self, Attachments},
  platform::{ApellBody, ApellPlatform, PlatformError},
  sse::SseParser,
  tokn_config, validation, wire, Failure, LlmConfig, LlmEvent, LlmMediaFile, LlmMediaOutput,
  LlmProvenance, LlmRequest, LlmResult, LlmStatus, LlmToolCall, LlmToolChoice, MAX_ERROR_BYTES,
  MAX_INPUT_BYTES, MAX_OUTPUT_BYTES, MAX_RESPONSE_BYTES, MAX_TOOLS,
};
use reqwest::Client;
use serde_json::{json, Map, Value};
use std::{
  collections::{BTreeMap, HashSet},
  time::Duration,
};

pub(super) struct Output {
  request_id: String,
  pub content: String,
  pub tool_calls: Vec<LlmToolCall>,
  pub provenance: LlmProvenance,
  pub(super) generated: Vec<generated::Asset>,
  pub(super) media: Vec<LlmMediaFile>,
  message: Map<String, Value>,
  finish_reason: Option<String>,
  response_id: Option<String>,
  calls: BTreeMap<usize, Value>,
  used_bytes: usize,
  refusal: bool,
  native: native::NativeOutput,
}

impl Output {
  pub fn new(request_id: &str, provenance: LlmProvenance) -> Self {
    Self {
      request_id: request_id.into(),
      content: String::new(),
      tool_calls: Vec::new(),
      provenance,
      generated: Vec::new(),
      media: Vec::new(),
      message: json!({"role": "assistant", "content": null})
        .as_object()
        .unwrap()
        .clone(),
      finish_reason: None,
      response_id: None,
      calls: BTreeMap::new(),
      used_bytes: 0,
      refusal: false,
      native: native::NativeOutput::default(),
    }
  }

  fn charge(&mut self, bytes: usize) -> Result<(), Failure> {
    self.used_bytes = self.used_bytes.saturating_add(bytes);
    if self.used_bytes > MAX_OUTPUT_BYTES {
      return Err(Failure::new(
        "response_limit",
        "The LLM output exceeded the size limit.",
      ));
    }
    Ok(())
  }

  fn text(&mut self, text: &str, emit: &impl Fn(LlmEvent)) {
    self.content.push_str(text);
    if !text.is_empty() {
      emit(event(&self.request_id, "text_delta", text));
    }
  }

  fn metadata(&mut self, value: &Value) -> Result<(), Failure> {
    if let Some(model) = value.get("model").filter(|value| !value.is_null()) {
      let model = model
        .as_str()
        .filter(|model| {
          !model.is_empty() && model.len() <= 256 && !model.chars().any(char::is_control)
        })
        .ok_or_else(invalid_response)?;
      if self
        .provenance
        .model_id
        .as_deref()
        .is_some_and(|previous| previous != model)
      {
        return Err(invalid_response());
      }
      self.provenance.model_id = Some(model.into());
    }
    if let Some(id) = value.get("id").filter(|value| !value.is_null()) {
      let id = id
        .as_str()
        .filter(|id| validation::valid_call_id(id))
        .ok_or_else(invalid_response)?;
      if self
        .response_id
        .as_deref()
        .is_some_and(|previous| previous != id)
      {
        return Err(invalid_response());
      }
      self.response_id = Some(id.into());
    }
    Ok(())
  }

  fn fields(
    &mut self,
    message: &Value,
    streaming: bool,
    emit: &impl Fn(LlmEvent),
  ) -> Result<(), Failure> {
    let object = message.as_object().ok_or_else(invalid_response)?;
    if object
      .get("role")
      .is_some_and(|role| role.as_str() != Some("assistant"))
      || (!streaming && object.get("role").and_then(Value::as_str) != Some("assistant"))
    {
      return Err(invalid_response());
    }
    self.charge(
      serde_json::to_vec(message)
        .map_err(|_| invalid_response())?
        .len(),
    )?;
    if object
      .get("function_call")
      .is_some_and(|value| !value.is_null())
    {
      return Err(Failure::unsupported("legacy_function_call"));
    }
    if let Some(value) = object.get("content").filter(|value| !value.is_null()) {
      self.text(value.as_str().ok_or_else(invalid_response)?, emit);
    }
    if let Some(value) = object.get("refusal").filter(|value| !value.is_null()) {
      let refusal = value.as_str().ok_or_else(invalid_response)?;
      self.refusal |= !refusal.is_empty();
      self.text(refusal, emit);
    }
    for name in ["reasoning_content", "reasoning"] {
      if let Some(text) = object
        .get(name)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
      {
        emit(event(&self.request_id, "reasoning_delta", text));
      }
    }
    if object.contains_key("provider_state") {
      return Err(Failure {
        message: "The Chat provider returned provider_state, which is reserved for Apell's native replay envelope.".into(),
        ..Failure::unsupported("reserved_provider_state")
      });
    }
    if let Some(calls) = object.get("tool_calls").filter(|value| !value.is_null()) {
      if calls.as_array().is_none_or(|calls| calls.len() > MAX_TOOLS) {
        return Err(invalid_response());
      }
    }
    if !streaming {
      // The entire message is applet data: retain arbitrary fields and nested
      // metadata exactly, rather than projecting it through a lossy schema.
      self.message = object.clone();
      if let Some(items) = object.get("reasoning_details").and_then(Value::as_array) {
        for item in items {
          for name in ["text", "summary"] {
            if let Some(text) = item
              .get(name)
              .and_then(Value::as_str)
              .filter(|text| !text.is_empty())
            {
              emit(event(&self.request_id, "reasoning_delta", text));
            }
          }
        }
      }
      return Ok(());
    }
    for (name, value) in object {
      if value.is_null() {
        self.message.entry(name).or_insert(Value::Null);
        continue;
      }
      match name.as_str() {
        "role" => {}
        "content" | "refusal" | "reasoning_content" | "reasoning" => {
          append_string(&mut self.message, name, value)?;
        }
        "tool_calls" => {
          for value in value.as_array().ok_or_else(invalid_response)? {
            let index = value
              .get("index")
              .and_then(Value::as_u64)
              .filter(|index| *index < MAX_TOOLS as u64)
              .ok_or_else(invalid_response)? as usize;
            self.tool_delta(index, value)?;
          }
          self.message.insert(
            name.clone(),
            Value::Array(self.calls.values().cloned().collect()),
          );
        }
        "annotations" | "reasoning_details" if value.as_array().is_some_and(Vec::is_empty) => {
          self.message.insert(name.clone(), value.clone());
        }
        // An unknown field may be a delta, a snapshot, or an indexed update.
        // Guessing a merge loses replay data; a buffered request preserves it.
        _ => return Err(stream_metadata()),
      }
    }
    Ok(())
  }

  fn tool_delta(&mut self, index: usize, value: &Value) -> Result<(), Failure> {
    let fields = value.as_object().ok_or_else(invalid_response)?;
    let call = self.calls.entry(index).or_insert_with(|| json!({}));
    let call = call.as_object_mut().expect("tool delta creates objects");
    for (name, value) in fields {
      match name.as_str() {
        "index" => {}
        "id" | "type" if !value.is_null() => {
          let text = value.as_str().ok_or_else(invalid_response)?;
          if name == "type" && text != "function" {
            return Err(Failure::unsupported("tool_type"));
          }
          if call
            .get(name)
            .is_some_and(|previous| !previous.is_null() && previous != value)
          {
            return Err(invalid_response());
          }
          call.insert(name.clone(), value.clone());
        }
        "function" if !value.is_null() => {
          let fields = value.as_object().ok_or_else(invalid_response)?;
          let function = call.entry("function").or_insert_with(|| json!({}));
          if function.is_null() {
            *function = json!({});
          }
          let function = function.as_object_mut().ok_or_else(invalid_response)?;
          for (name, value) in fields {
            if value.is_null() {
              function.entry(name).or_insert(Value::Null);
            } else if matches!(name.as_str(), "name" | "arguments") {
              append_string(function, name, value)?;
            } else {
              return Err(stream_metadata());
            }
          }
        }
        _ if value.is_null() => {
          call.entry(name).or_insert(Value::Null);
        }
        _ => return Err(stream_metadata()),
      }
    }
    Ok(())
  }

  fn consume(
    &mut self,
    value: &Value,
    streaming: bool,
    emit: &impl Fn(LlmEvent),
  ) -> Result<(), Failure> {
    if value.get("error").is_some() {
      return Err(provider_error(value, None));
    }
    self.metadata(value)?;
    let Some(choices) = value.get("choices").and_then(Value::as_array) else {
      return Ok(());
    };
    if choices.is_empty() {
      return Ok(());
    }
    if choices.len() != 1 {
      return Err(Failure::unsupported("multiple_choices"));
    }
    let choice = &choices[0];
    if choice
      .get("index")
      .is_some_and(|index| index.as_u64() != Some(0))
    {
      return Err(invalid_response());
    }
    let body = if streaming {
      &choice["delta"]
    } else {
      &choice["message"]
    };
    if self.finish_reason.is_some() && body.as_object().is_some_and(|body| !body.is_empty()) {
      return Err(invalid_response());
    }
    if !body.is_null() {
      self.fields(body, streaming, emit)?;
    }
    if let Some(reason) = choice.get("finish_reason").filter(|value| !value.is_null()) {
      let reason = reason
        .as_str()
        .filter(|reason| reason.len() <= 128 && !reason.chars().any(char::is_control))
        .ok_or_else(invalid_response)?;
      if self
        .finish_reason
        .as_deref()
        .is_some_and(|previous| previous != reason)
      {
        return Err(invalid_response());
      }
      self.finish_reason = Some(reason.into());
    }
    Ok(())
  }

  fn buffered(
    &mut self,
    mut value: Value,
    config: &LlmConfig,
    protocol: wire::Protocol,
    request: &LlmRequest,
    emit: &impl Fn(LlmEvent),
  ) -> Result<LlmStatus, Failure> {
    // Keep inline bytes out of the assistant message and its metadata budget.
    // Validate the terminal response before decoding any generated files.
    let items = match protocol {
      wire::Protocol::ChatCompletions => {
        let audio = take_chat_audio(&mut value, request.output.as_ref());
        self.consume(&value, false, emit)?;
        audio.into_iter().collect()
      }
      wire::Protocol::Responses => {
        let images = native::take_response_images(&mut value);
        self.responses(&value, false, config, emit)?;
        images
      }
      wire::Protocol::Messages => {
        self.messages(&value, false, config, emit)?;
        Vec::new()
      }
    };
    let status = self.finish(request)?;
    if !matches!(status, LlmStatus::Completed | LlmStatus::ToolCalls) {
      return Ok(status);
    }
    let mut assets = Vec::new();
    match (&request.output, protocol) {
      (Some(LlmMediaOutput::Image { format }), wire::Protocol::Responses) => {
        for item in items {
          generated::decode_image(&item, *format, &mut assets)?;
        }
      }
      (Some(LlmMediaOutput::Audio { format, .. }), wire::Protocol::ChatCompletions) => {
        if let Some(audio) = items.first() {
          generated::decode_audio(audio, *format, &mut assets)?;
        }
      }
      _ if !items.is_empty() => return Err(Failure::unsupported("media_output")),
      _ => {}
    }
    if request.output.is_some() && status == LlmStatus::Completed && assets.is_empty() {
      return Err(Failure::new(
        "missing_media",
        "The provider completed without returning the requested media.",
      ));
    }
    self.generated = assets;
    Ok(status)
  }

  fn finish(&mut self, request: &LlmRequest) -> Result<LlmStatus, Failure> {
    if self.refusal {
      return Err(Failure::new(
        "refusal",
        "The provider declined the request.",
      ));
    }
    match self.finish_reason.as_deref() {
      Some("length") => return Ok(LlmStatus::Truncated),
      Some("content_filter") => {
        return Err(Failure::new(
          "content_filter",
          "The provider stopped the response because of its content policy.",
        ))
      }
      Some("stop" | "tool_calls") => {}
      Some(_) => {
        return Err(Failure::new(
          "unknown_finish_reason",
          "The provider returned an unsupported completion reason.",
        ))
      }
      None => {
        return Err(Failure::new(
          "incomplete_response",
          "The LLM stream ended before the answer was complete.",
        ))
      }
    }
    if serde_json::to_vec(&self.message)
      .map_err(|_| invalid_response())?
      .len()
      > MAX_OUTPUT_BYTES
    {
      return Err(Failure::new(
        "response_limit",
        "The LLM output exceeded the size limit.",
      ));
    }
    self.tool_calls = validation::message_fields(&self.message).map_err(|error| {
      if error.code == "unsupported_capability" {
        error
      } else {
        invalid_response()
      }
    })?;
    if self.finish_reason.as_deref() == Some("tool_calls") {
      if self.tool_calls.is_empty() {
        return Err(invalid_response());
      }
      let mut ids = HashSet::new();
      for call in &self.tool_calls {
        if !validation::valid_call_id(&call.id)
          || !validation::valid_tool_name(&call.name)
          || !ids.insert(call.id.as_str())
          || !request.tools.iter().any(|tool| tool.name == call.name)
        {
          return Err(Failure::new(
            "invalid_tool_call",
            "The provider returned an invalid or undeclared tool call.",
          ));
        }
      }
      match &request.tool_choice {
        Some(LlmToolChoice::Mode(mode)) if mode == "none" => {
          return Err(Failure::new(
            "tool_choice_not_honored",
            "The provider returned tools when tool choice was none.",
          ))
        }
        Some(LlmToolChoice::Named { name })
          if self.tool_calls.iter().any(|call| &call.name != name) =>
        {
          return Err(Failure::new(
            "tool_choice_not_honored",
            "The provider did not use the selected tool.",
          ))
        }
        _ => {}
      }
      return Ok(LlmStatus::ToolCalls);
    }
    if !self.tool_calls.is_empty() {
      return Err(invalid_response());
    }
    if matches!(&request.tool_choice, Some(LlmToolChoice::Named { .. }))
      || matches!(&request.tool_choice, Some(LlmToolChoice::Mode(mode)) if mode == "required")
    {
      return Err(Failure::new(
        "tool_choice_not_honored",
        "The provider completed without the required tool call.",
      ));
    }
    if self.content.is_empty() && request.output.is_none() {
      return Err(Failure::new(
        "empty_response",
        "The LLM provider returned no answer.",
      ));
    }
    Ok(LlmStatus::Completed)
  }

  pub fn result(self, status: Result<LlmStatus, Failure>) -> LlmResult {
    let (status, error) = match status {
      Ok(LlmStatus::Truncated) => (
        LlmStatus::Truncated,
        Some(Failure::new(
          "truncated",
          "The provider stopped before the answer was complete.",
        )),
      ),
      Ok(status) => (status, None),
      Err(error) => (
        match error.code.as_str() {
          "cancelled" => LlmStatus::Cancelled,
          "unsupported_capability" => LlmStatus::Unsupported,
          _ => LlmStatus::Failed,
        },
        Some(error),
      ),
    };
    let successful = matches!(status, LlmStatus::Completed | LlmStatus::ToolCalls);
    LlmResult {
      request_id: self.request_id,
      status,
      content: self.content,
      tool_calls: if status == LlmStatus::ToolCalls {
        self.tool_calls
      } else {
        Vec::new()
      },
      message: successful.then_some(Value::Object(self.message)),
      media: if successful { self.media } else { Vec::new() },
      finish_reason: self.finish_reason,
      provenance: self.provenance,
      error,
    }
  }
}

fn take_chat_audio(response: &mut Value, output: Option<&LlmMediaOutput>) -> Option<Value> {
  let message = response
    .pointer_mut("/choices/0/message")?
    .as_object_mut()?;
  if message.get("audio")?.is_null() {
    return None;
  }
  let audio = message.remove("audio")?;
  if matches!(output, Some(LlmMediaOutput::Audio { .. }))
    && message
      .get("content")
      .is_none_or(|value| value.is_null() || value.as_str() == Some(""))
  {
    if let Some(transcript) = audio.get("transcript").and_then(Value::as_str) {
      message.insert("content".into(), json!(transcript));
    }
  }
  Some(audio)
}

fn append_string(
  target: &mut Map<String, Value>,
  name: &str,
  value: &Value,
) -> Result<(), Failure> {
  let text = value.as_str().ok_or_else(stream_metadata)?;
  let current = target.entry(name).or_insert(Value::Null);
  if current.is_null() {
    *current = Value::String(String::new());
  }
  current.as_str().ok_or_else(invalid_response)?;
  if let Value::String(current) = current {
    current.push_str(text);
  }
  Ok(())
}

fn stream_metadata() -> Failure {
  Failure {
    message: "The provider streamed metadata that cannot be reconstructed safely. Use stream: false to retain its complete assistant message.".into(),
    ..Failure::unsupported("stream_metadata")
  }
}

fn invalid_response() -> Failure {
  Failure::new(
    "invalid_response",
    "The provider returned an invalid or inconsistent response.",
  )
}

fn provider_error(value: &Value, status: Option<u16>) -> Failure {
  let error = &value["error"];
  let code = error.get("code").and_then(Value::as_str).unwrap_or("");
  let parameter = error.get("param").and_then(Value::as_str).unwrap_or("");
  if matches!(
    code,
    "unsupported_parameter" | "unsupported_value" | "unsupported_feature"
  ) {
    let capability = match parameter {
      "tools" | "tools.function" => Some("tools"),
      "tool_choice" => Some("required_tool_choice"),
      "reasoning" | "reasoning_content" => Some("exposed_reasoning"),
      "max_tokens" | "max_completion_tokens" | "max_output_tokens" => Some("max_output_tokens"),
      "audio" | "audio.format" | "audio.voice" | "modalities" => Some("audio_output"),
      "image_generation" | "tools.image_generation" => Some("image_output"),
      _ => None,
    };
    if let Some(capability) = capability {
      return Failure::unsupported(capability);
    }
  }
  let message = status
    .map(|status| format!("The LLM provider returned HTTP {status}."))
    .unwrap_or_else(|| "The LLM provider rejected the request.".into());
  Failure {
    retryable: status.is_some_and(|status| status == 429 || status >= 500),
    ..Failure::new("provider_error", &message)
  }
}

pub(super) async fn demo(
  request: &LlmRequest,
  output: &mut Output,
  emit: &impl Fn(LlmEvent),
) -> Result<LlmStatus, Failure> {
  let prompt = request
    .messages
    .iter()
    .rev()
    .find(|message| message["role"] == "user")
    .map(|message| match media::parts(message) {
      Some(parts) => parts
        .iter()
        .filter_map(|part| part["text"].as_str())
        .flat_map(str::chars)
        .take(200)
        .collect::<String>(),
      None => message["content"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect(),
    })
    .unwrap_or_default();
  let answer = format!("This is the local demo provider. Your message was: “{prompt}”\n\nThe applet is running Rust WebAssembly, and this conversation is stored by the shell’s native SQLite service. Configure a provider in the shell settings for real model responses.");
  for text in answer.split_inclusive(' ') {
    tokio::time::sleep(Duration::from_millis(18)).await;
    output.charge(text.len())?;
    if request.stream {
      output.text(text, emit);
    } else {
      output.content.push_str(text);
    }
  }
  output
    .message
    .insert("content".into(), json!(output.content));
  output.finish_reason = Some("stop".into());
  output.finish(request)
}

pub(super) async fn remote(
  client: &Client,
  request: &LlmRequest,
  config: &LlmConfig,
  account: Option<tokn_sdk::account::AccountConfig>,
  attachments: Attachments,
  output: &mut Output,
  emit: &impl Fn(LlmEvent),
) -> Result<LlmStatus, Failure> {
  let protocol = wire::protocol(config)?;
  let max_wire_bytes = if attachments.is_empty() {
    MAX_INPUT_BYTES
  } else {
    // The 8 MiB raw file budget expands under base64. Bound the entire body
    // independently to account for context and any provider wire transforms.
    12 * 1024 * 1024
  };
  let body = wire::request(request, config, protocol, &attachments)?;
  drop(attachments);
  let progress = |event| {
    if request.stream && request.output.is_none() {
      emit(event);
    }
  };
  if serde_json::to_vec(&body)
    .map_err(|_| Failure::new("invalid_request", "The LLM request is not valid JSON."))?
    .len()
    > max_wire_bytes
  {
    return Err(Failure::new(
      "context_limit",
      "The context including tool schemas and message metadata exceeds the size limit.",
    ));
  }
  let rules = tokn_config::execution_rules(config).map_err(|_| {
    Failure::new(
      "invalid_configuration",
      "The LLM connection configuration is invalid.",
    )
  })?;
  let response_limit = if request.output.is_some() {
    // The decoded file budget is independent of the base64 transport overhead.
    12 * 1024 * 1024
  } else {
    MAX_RESPONSE_BYTES
  };
  let sdk = tokn_sdk::Client::new(
    rules,
    Vec::new(),
    ApellPlatform::new(client.clone(), response_limit),
  )
  .map_err(|_| {
    Failure::new(
      "invalid_configuration",
      "Could not configure the LLM connection.",
    )
  })?
  .with_limits(tokn_sdk::Limits {
    max_response_bytes: response_limit,
    max_event_bytes: MAX_OUTPUT_BYTES,
    max_error_body_bytes: MAX_ERROR_BYTES,
    ..tokn_sdk::Limits::default()
  });
  let options = tokn_config::options(
    config,
    account.as_ref(),
    protocol,
    &body,
    &request.request_id,
    tokn_core::util::initiator::classify_initiator(&json!({ "messages": request.messages }))
      .unwrap_or(""),
  )?;
  // The embedded fixed-provider relay preserves the compatible wire format.
  // Friendly generation currently canonicalizes through Responses, which can
  // discard assistant metadata the applet needs to preserve in its history.
  let response = sdk
    .execute(protocol.endpoint(), body, options)
    .await
    .map_err(platform_failure)?;
  let status = response.status();
  let content_type = response.content_type().unwrap_or("").to_ascii_lowercase();
  let response_body = response.into_body();
  if !(200..300).contains(&status) {
    let bytes = match response_body {
      ApellBody::Buffered(bytes) => bytes,
      ApellBody::Stream(mut stream) => {
        let mut bytes = Vec::new();
        while let Ok(Some(chunk)) = stream.next_chunk().await {
          if bytes.len().saturating_add(chunk.len()) > MAX_ERROR_BYTES {
            break;
          }
          bytes.extend_from_slice(&chunk);
        }
        bytes
      }
    };
    return Err(provider_error(
      &if bytes.len() <= MAX_ERROR_BYTES {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
      } else {
        Value::Null
      },
      Some(status),
    ));
  }
  match response_body {
    ApellBody::Buffered(bytes) if content_type.starts_with("application/json") => {
      let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid_response())?;
      return output.buffered(value, config, protocol, request, &progress);
    }
    ApellBody::Stream(_) if request.output.is_some() => {
      return Err(Failure::unsupported("media_streaming"));
    }
    ApellBody::Stream(mut stream) if content_type.starts_with("text/event-stream") => {
      let mut parser = SseParser::default();
      let mut received = 0usize;
      // Opaque SDK chunks retain error envelopes and private metadata. Its
      // semantic event API would consume in-stream errors before Apell could
      // classify them and would require [DONE] even after an explicit finish.
      'stream: while let Some(chunk) = stream.next_chunk().await.map_err(platform_failure)? {
        received = received.saturating_add(chunk.len());
        if received > MAX_RESPONSE_BYTES {
          return Err(Failure::new(
            "response_limit",
            "The LLM response exceeded the size limit.",
          ));
        }
        for data in parser.push(&chunk)? {
          if data.trim() == "[DONE]" {
            break 'stream;
          }
          let value: Value = serde_json::from_str(&data).map_err(|_| invalid_response())?;
          match protocol {
            wire::Protocol::ChatCompletions => output.consume(&value, true, &progress)?,
            wire::Protocol::Responses => output.responses(&value, true, config, &progress)?,
            wire::Protocol::Messages => output.messages(&value, true, config, &progress)?,
          }
          if output.native.terminal {
            break 'stream;
          }
        }
      }
      // Unframed trailing records are discarded. [DONE] alone is not evidence
      // of successful completion; the provider must supply a finish reason.
    }
    _ => return Err(Failure::unsupported("response_format")),
  }
  output.finish(request)
}

fn platform_failure(error: PlatformError) -> Failure {
  match error {
    PlatformError::Transport => Failure::transport("The LLM request or response was interrupted."),
    PlatformError::Limit => Failure::new(
      "response_limit",
      "The LLM response exceeded the size limit.",
    ),
    PlatformError::Protocol => invalid_response(),
  }
}

mod native;

#[cfg(test)]
mod tests;

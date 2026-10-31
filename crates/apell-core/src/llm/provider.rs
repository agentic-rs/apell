use super::{
  event,
  platform::{ApellBody, ApellPlatform, PlatformError},
  sse::SseParser,
  tokn_config, validation, Failure, LlmConfig, LlmEvent, LlmProvenance, LlmRequest, LlmResult,
  LlmStatus, LlmToolCall, LlmToolChoice, MAX_ERROR_BYTES, MAX_INPUT_BYTES, MAX_OUTPUT_BYTES,
  MAX_RESPONSE_BYTES, MAX_TOOLS,
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
  pub opaque: Map<String, Value>,
  pub provenance: LlmProvenance,
  pub continuation_id: Option<String>,
  finish_reason: Option<String>,
  response_id: Option<String>,
  calls: BTreeMap<usize, LlmToolCall>,
  used_bytes: usize,
  refusal: bool,
}

impl Output {
  pub fn new(request_id: &str, provenance: LlmProvenance) -> Self {
    Self {
      request_id: request_id.into(),
      content: String::new(),
      tool_calls: Vec::new(),
      opaque: Map::new(),
      provenance,
      continuation_id: None,
      finish_reason: None,
      response_id: None,
      calls: BTreeMap::new(),
      used_bytes: 0,
      refusal: false,
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

  fn text(&mut self, text: &str, emit: &impl Fn(LlmEvent)) -> Result<(), Failure> {
    self.charge(text.len())?;
    self.content.push_str(text);
    if !text.is_empty() {
      emit(event(&self.request_id, "delta", text));
    }
    Ok(())
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
    {
      return Err(invalid_response());
    }
    for (name, value) in object {
      if value.is_null() {
        continue;
      }
      if !matches!(
        name.as_str(),
        "role"
          | "content"
          | "refusal"
          | "tool_calls"
          | "function_call"
          | "annotations"
          | "reasoning_content"
          | "reasoning"
          | "reasoning_details"
      ) {
        return Err(Failure::unsupported("continuation_format"));
      }
    }
    if let Some(value) = object.get("content").filter(|value| !value.is_null()) {
      self.text(value.as_str().ok_or_else(invalid_response)?, emit)?;
    }
    if let Some(value) = object.get("refusal").filter(|value| !value.is_null()) {
      let refusal = value.as_str().ok_or_else(invalid_response)?;
      self.refusal |= !refusal.is_empty();
      // A refusal remains a distinct failed outcome, but its public text is
      // useful partial content. Never confuse it with a transport failure.
      self.text(refusal, emit)?;
    }
    for field in ["reasoning_content", "reasoning"] {
      if let Some(value) = object.get(field).filter(|value| !value.is_null()) {
        let text = value
          .as_str()
          .ok_or_else(|| Failure::unsupported("continuation_format"))?;
        self.charge(text.len())?;
        let entry = self.opaque.entry(field).or_insert_with(|| json!(""));
        if let Value::String(accumulated) = entry {
          accumulated.push_str(text);
        }
        if !text.is_empty() {
          emit(event(&self.request_id, "reasoning_delta", text));
        }
      }
    }
    if let Some(details) = object
      .get("reasoning_details")
      .filter(|value| !value.is_null())
    {
      if details.as_array().is_some_and(Vec::is_empty) {
        // An explicitly empty field can still be required on provider replay.
        self.charge(2)?;
        self
          .opaque
          .insert("reasoning_details".into(), details.clone());
      } else if streaming {
        return Err(Failure::unsupported("continuation_format"));
      } else {
        self.charge(
          serde_json::to_vec(details)
            .map_err(|_| invalid_response())?
            .len(),
        )?;
        self
          .opaque
          .insert("reasoning_details".into(), details.clone());
        if let Some(items) = details.as_array() {
          for item in items {
            for name in ["text", "summary"] {
              if let Some(text) = item.get(name).and_then(Value::as_str) {
                if !text.is_empty() {
                  emit(event(&self.request_id, "reasoning_delta", text));
                }
              }
            }
          }
        }
      }
    }
    if let Some(calls) = object.get("tool_calls").filter(|value| !value.is_null()) {
      let calls = calls.as_array().ok_or_else(invalid_response)?;
      if calls.len() > MAX_TOOLS {
        return Err(invalid_response());
      }
      for (position, value) in calls.iter().enumerate() {
        let index = if streaming {
          value
            .get("index")
            .and_then(Value::as_u64)
            .filter(|index| *index < MAX_TOOLS as u64)
            .ok_or_else(invalid_response)? as usize
        } else {
          position
        };
        self.tool_delta(index, value, streaming)?;
      }
    }
    if object
      .get("function_call")
      .is_some_and(|value| !value.is_null())
    {
      return Err(Failure::unsupported("legacy_function_call"));
    }
    Ok(())
  }

  fn tool_delta(&mut self, index: usize, value: &Value, streaming: bool) -> Result<(), Failure> {
    let fields = value.as_object().ok_or_else(invalid_response)?;
    if fields.iter().any(|(name, value)| {
      !value.is_null() && !matches!(name.as_str(), "index" | "id" | "type" | "function")
    }) {
      return Err(Failure::unsupported("continuation_format"));
    }
    if value
      .get("type")
      .is_some_and(|value| value.as_str() != Some("function"))
    {
      return Err(Failure::unsupported("tool_type"));
    }
    let function = value
      .get("function")
      .and_then(Value::as_object)
      .ok_or_else(invalid_response)?;
    if function
      .iter()
      .any(|(name, value)| !value.is_null() && !matches!(name.as_str(), "name" | "arguments"))
    {
      return Err(Failure::unsupported("continuation_format"));
    }
    let id = value
      .get("id")
      .map(|value| value.as_str().ok_or_else(invalid_response))
      .transpose()?;
    let name = function
      .get("name")
      .map(|value| value.as_str().ok_or_else(invalid_response))
      .transpose()?;
    let arguments = function
      .get("arguments")
      .map(|value| value.as_str().ok_or_else(invalid_response))
      .transpose()?
      .unwrap_or("");
    self.charge(id.map_or(0, str::len) + name.map_or(0, str::len) + arguments.len())?;
    let call = self.calls.entry(index).or_insert_with(|| LlmToolCall {
      id: String::new(),
      name: String::new(),
      arguments: String::new(),
    });
    if let Some(id) = id {
      if !call.id.is_empty() && call.id != id {
        return Err(invalid_response());
      }
      call.id = id.into();
    }
    if let Some(name) = name {
      if !call.name.is_empty() && call.name != name {
        return Err(invalid_response());
      }
      call.name = name.into();
    }
    if !streaming
      && (!validation::valid_call_id(&call.id) || !validation::valid_tool_name(&call.name))
    {
      return Err(invalid_response());
    }
    call.arguments.push_str(arguments);
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

  fn finish(&mut self, request: &LlmRequest) -> Result<LlmStatus, Failure> {
    self.tool_calls = self.calls.values().cloned().collect();
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
    if self.content.is_empty() {
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
    LlmResult {
      request_id: self.request_id,
      status,
      content: self.content,
      tool_calls: if status == LlmStatus::ToolCalls {
        self.tool_calls
      } else {
        Vec::new()
      },
      continuation_id: self.continuation_id,
      finish_reason: self.finish_reason,
      provenance: self.provenance,
      error,
    }
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
    .find(|message| message.role == "user")
    .map(|message| message.content.chars().take(200).collect::<String>())
    .unwrap_or_default();
  let answer = format!("This is the local demo provider. Your message was: “{prompt}”\n\nThe applet is running Rust WebAssembly, and this conversation is stored by the shell’s native SQLite service. Configure a provider in the shell settings for real model responses.");
  for text in answer.split_inclusive(' ') {
    tokio::time::sleep(Duration::from_millis(18)).await;
    output.text(text, emit)?;
  }
  output.finish_reason = Some("stop".into());
  output.finish(request)
}

pub(super) async fn remote(
  client: &Client,
  request: &LlmRequest,
  config: &LlmConfig,
  api_key: Option<String>,
  messages: Vec<Value>,
  output: &mut Output,
  emit: &impl Fn(LlmEvent),
) -> Result<LlmStatus, Failure> {
  let mut body = json!({"model": config.model_id, "messages": messages, "stream": true});
  if let Some(temperature) = request.temperature {
    body["temperature"] = json!(temperature);
  }
  if !request.tools.is_empty() {
    body["tools"] = Value::Array(
      request
        .tools
        .iter()
        .map(|tool| {
          let mut function = json!({"name": tool.name, "parameters": tool.parameters});
          if let Some(description) = &tool.description {
            function["description"] = json!(description);
          }
          json!({"type": "function", "function": function})
        })
        .collect(),
    );
  }
  if let Some(choice) = &request.tool_choice {
    body["tool_choice"] = match choice {
      LlmToolChoice::Mode(mode) => json!(mode),
      LlmToolChoice::Named { name } => json!({"type": "function", "function": {"name": name}}),
    };
  }
  if serde_json::to_vec(&body)
    .map_err(|_| Failure::new("invalid_request", "The LLM request is not valid JSON."))?
    .len()
    > MAX_INPUT_BYTES
  {
    return Err(Failure::new(
      "context_limit",
      "The context including tool schemas and continuation data exceeds the size limit.",
    ));
  }
  let rules = tokn_config::execution_rules(config).map_err(|_| {
    Failure::new(
      "invalid_configuration",
      "The LLM connection configuration is invalid.",
    )
  })?;
  let sdk = tokn_sdk::Client::new(rules, Vec::new(), ApellPlatform::new(client.clone()))
    .map_err(|_| {
      Failure::new(
        "invalid_configuration",
        "Could not configure the LLM connection.",
      )
    })?
    .with_limits(tokn_sdk::Limits {
      max_response_bytes: MAX_RESPONSE_BYTES,
      max_event_bytes: MAX_OUTPUT_BYTES,
      max_error_body_bytes: MAX_ERROR_BYTES,
    });
  let mut options = tokn_sdk::RequestOptions::default().with_request_id(&request.request_id);
  if let Some(key) = api_key.filter(|key| !key.is_empty()) {
    options = options.with_header("authorization", format!("Bearer {key}"));
  }
  // The embedded fixed-provider relay preserves the compatible wire format.
  // Friendly generation currently canonicalizes through Responses, which can
  // discard provider-private assistant metadata required by continuation.
  let response = sdk
    .execute(tokn_sdk::Endpoint::ChatCompletions, body, options)
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
      output.consume(&value, false, emit)?;
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
          output.consume(&value, true, emit)?;
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

#[cfg(test)]
mod tests;

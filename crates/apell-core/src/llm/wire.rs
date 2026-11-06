use super::{media, validation, Failure, LlmConfig, LlmRequest, LlmToolCall, LlmToolChoice};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokn_convert::{error::ConvertError, media::content_to_parts};
use tokn_sdk::{Content, Endpoint};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Protocol {
  ChatCompletions,
  Responses,
  Messages,
}

impl Protocol {
  pub fn endpoint(self) -> Endpoint {
    match self {
      Self::ChatCompletions => Endpoint::ChatCompletions,
      Self::Responses => Endpoint::Responses,
      Self::Messages => Endpoint::Messages,
    }
  }

  pub(super) fn supports_audio_input(self) -> bool {
    self == Self::ChatCompletions
  }
}

pub(super) fn driver(config: &LlmConfig) -> Result<&str, String> {
  match config.provider_id.as_str() {
    "custom" => Ok("openai"),
    "openai"
    | "codex"
    | "github-copilot"
    | "deepseek"
    | "llama-cpp"
    | "opencode-go"
    | "zai"
    | "zai-coding-plan"
    | "zhipuai"
    | "zhipuai-coding-plan" => Ok(&config.provider_id),
    _ => Err("Select a supported provider.".into()),
  }
}

pub(super) fn protocol(config: &LlmConfig) -> Result<Protocol, Failure> {
  let driver = driver(config)
    .map_err(|_| Failure::new("invalid_configuration", "The provider is invalid."))?;
  let candidates: &[Protocol] = match config.provider_id.as_str() {
    "openai" | "codex" => &[Protocol::Responses],
    "github-copilot" | "opencode-go" => &[
      Protocol::Responses,
      Protocol::Messages,
      Protocol::ChatCompletions,
    ],
    _ => &[Protocol::ChatCompletions],
  };
  candidates
    .iter()
    .copied()
    .find(|protocol| {
      tokn_accounts::native::supports_model_endpoint(
        driver,
        &config.model_id,
        protocol.endpoint(),
        &config.base_url,
      )
    })
    .ok_or_else(|| Failure::unsupported("model_protocol"))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderState {
  version: u8,
  protocol: Protocol,
  provider_id: String,
  connection_id: String,
  model_id: String,
  output: Vec<Value>,
}

pub(super) struct Projection {
  pub content: String,
  pub calls: Vec<LlmToolCall>,
  pub reasoning: Vec<String>,
  pub refusal: bool,
}

fn invalid() -> Failure {
  Failure::new(
    "invalid_response",
    "The provider returned invalid native output.",
  )
}

fn mismatch() -> Failure {
  Failure::new("provider_state_mismatch", "This assistant message belongs to a different provider, connection, model, or protocol, or its display fields changed. Start a fresh context or restore the original message.")
}

/// Native replay records are applet-owned data, never references into host memory.
pub(super) fn message(
  config: &LlmConfig,
  protocol: Protocol,
  output: Vec<Value>,
) -> Result<Value, Failure> {
  let projection = project(protocol, &output)?;
  let mut message = json!({
    "role":"assistant",
    "content": if projection.content.is_empty() { Value::Null } else { json!(projection.content) },
    "provider_state": ProviderState {
      version:1, protocol, provider_id:config.provider_id.clone(), connection_id:config.connection_id.clone(),
      model_id:config.model_id.clone(), output,
    }
  });
  if !projection.calls.is_empty() {
    message["tool_calls"] = json!(projection
      .calls
      .iter()
      .map(|call| json!({
        "id":call.id, "type":"function", "function":{"name":call.name, "arguments":call.arguments}
      }))
      .collect::<Vec<_>>());
  }
  Ok(message)
}

fn replay(
  message: &Value,
  config: &LlmConfig,
  protocol: Protocol,
) -> Result<Option<Vec<Value>>, Failure> {
  let Some(state) = message.get("provider_state") else {
    return Ok(None);
  };
  let state: ProviderState = serde_json::from_value(state.clone()).map_err(|_| mismatch())?;
  if state.version != 1
    || state.protocol != protocol
    || state.provider_id != config.provider_id
    || state.connection_id != config.connection_id
    || state.model_id != config.model_id
    || protocol == Protocol::ChatCompletions
    || message["role"] != "assistant"
  {
    return Err(mismatch());
  }
  let projection = project(protocol, &state.output).map_err(|_| mismatch())?;
  let calls = validation::message_fields(message.as_object().ok_or_else(mismatch)?)?;
  if projection.refusal
    || projection.content != message["content"].as_str().unwrap_or("")
    || projection.calls != calls
  {
    return Err(mismatch());
  }
  if message.as_object().unwrap().keys().any(|key| {
    !matches!(
      key.as_str(),
      "role" | "content" | "tool_calls" | "provider_state"
    )
  }) {
    return Err(mismatch());
  }
  Ok(Some(state.output))
}

fn portable(message: &Value) -> Result<(), Failure> {
  let fields = message.as_object().ok_or_else(invalid)?;
  if fields.keys().any(|name| {
    !matches!(
      name.as_str(),
      "role" | "content" | "tool_calls" | "tool_call_id"
    )
  }) {
    return Err(Failure::unsupported("cross_protocol_metadata"));
  }
  if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
    for call in calls {
      if call.as_object().is_none_or(|fields| {
        fields
          .keys()
          .any(|name| !matches!(name.as_str(), "id" | "type" | "function"))
      }) || call
        .get("function")
        .and_then(Value::as_object)
        .is_none_or(|fields| {
          fields
            .keys()
            .any(|name| !matches!(name.as_str(), "name" | "arguments"))
        })
      {
        return Err(Failure::unsupported("cross_protocol_metadata"));
      }
    }
  }
  Ok(())
}

fn invalid_input_part() -> Failure {
  Failure::new("invalid_request", "Could not prepare the model input.")
}

fn structured_content(
  protocol: Protocol,
  message_index: usize,
  parts: &[Value],
  attachments: &media::Attachments,
) -> Result<Value, Failure> {
  let mut native = Vec::with_capacity(parts.len());
  for (part_index, part) in parts.iter().enumerate() {
    let text;
    let content = if part["type"] == "text" {
      text = Content::text(part["text"].as_str().ok_or_else(invalid_input_part)?);
      &text
    } else {
      attachments
        .get(&(message_index, part_index))
        .ok_or_else(invalid_input_part)?
    };
    let capability = match content {
      Content::Text(_) => None,
      Content::Image(_) => Some("image_input"),
      Content::Document(_) => Some("pdf_input"),
      Content::Audio(_) if protocol.supports_audio_input() => Some("audio_input"),
      Content::Audio(_) => return Err(Failure::unsupported("audio_input")),
      _ => return Err(invalid_input_part()),
    };
    let lowered =
      content_to_parts(protocol.endpoint(), content, "user").map_err(|error| {
        match (error, capability) {
          (ConvertError::UnsupportedFeature { .. }, Some(capability)) => {
            Failure::unsupported(capability)
          }
          _ => invalid_input_part(),
        }
      })?;
    native.extend(lowered);
  }
  Ok(Value::Array(native))
}

pub(super) fn request(
  request: &LlmRequest,
  config: &LlmConfig,
  protocol: Protocol,
  attachments: &media::Attachments,
) -> Result<Value, Failure> {
  let mut body = json!({"model":config.model_id, "messages":[], "stream":request.stream});
  if let Some(temperature) = request.temperature {
    body["temperature"] = json!(temperature);
  }
  if let Some(limit) = request.max_output_tokens {
    body["max_tokens"] = json!(limit);
  }
  if !request.tools.is_empty() {
    body["tools"] = json!(request
      .tools
      .iter()
      .map(|tool| {
        let mut function = json!({"name":tool.name, "parameters":tool.parameters});
        if let Some(description) = &tool.description {
          function["description"] = json!(description);
        }
        json!({"type":"function", "function":function})
      })
      .collect::<Vec<_>>());
  }
  if let Some(choice) = &request.tool_choice {
    body["tool_choice"] = match choice {
      LlmToolChoice::Mode(mode) => json!(mode),
      LlmToolChoice::Named { name } => json!({"type":"function", "function":{"name":name}}),
    };
  }
  if protocol == Protocol::ChatCompletions {
    // This name is reserved for Apell's native replay envelope, even when null.
    // Chat responses reject collisions before reporting a replayable success.
    if request
      .messages
      .iter()
      .any(|message| message.get("provider_state").is_some())
    {
      return Err(mismatch());
    }
    let mut messages = request.messages.clone();
    for (message_index, message) in messages.iter_mut().enumerate() {
      if let Some(parts) = media::parts(message) {
        message["content"] = structured_content(protocol, message_index, parts, attachments)?;
      }
    }
    body["messages"] = Value::Array(messages);
  } else {
    if config.provider_id == "codex" && request.max_output_tokens.is_some() {
      return Err(Failure::unsupported("max_output_tokens"));
    }
    body = tokn_convert::convert_request(Endpoint::ChatCompletions, protocol.endpoint(), &body)
      .map_err(|_| {
        Failure::new(
          "invalid_request",
          "Could not prepare the native model request.",
        )
      })?;
    let mut records = Vec::new();
    let mut instructions = Vec::new();
    for (message_index, message) in request.messages.iter().enumerate() {
      if let Some(output) = replay(message, config, protocol)? {
        if protocol == Protocol::Responses {
          records.extend(output);
        } else {
          for mut message in output {
            let fields = message.as_object_mut().ok_or_else(mismatch)?;
            // Messages responses carry an envelope around their replayable role
            // and blocks. Preserve that envelope in provider_state, not on wire.
            for key in [
              "id",
              "type",
              "model",
              "stop_reason",
              "stop_sequence",
              "usage",
            ] {
              fields.remove(key);
            }
            records.push(message);
          }
        }
        continue;
      }
      portable(message)?;
      if let Some(parts) = media::parts(message) {
        // Lower directly to this endpoint so media bytes do not pass through
        // the Chat conversion or touch provider-native replay records.
        records.push(json!({
          "role": "user",
          "content": structured_content(protocol, message_index, parts, attachments)?,
        }));
        continue;
      }
      let converted = tokn_convert::convert_request(
        Endpoint::ChatCompletions,
        protocol.endpoint(),
        &json!({
          "model":config.model_id, "messages":[message], "stream":request.stream
        }),
      )
      .map_err(|_| {
        Failure::new(
          "invalid_request",
          "Could not prepare a native conversation message.",
        )
      })?;
      let key = if protocol == Protocol::Responses {
        "input"
      } else {
        "messages"
      };
      if let Some(items) = converted.get(key).and_then(Value::as_array) {
        for item in items {
          // Messages requires all results for parallel calls in the next user
          // message. Portable tool results each convert to a separate user
          // block, so coalesce only those adjacent, metadata-free records.
          if protocol == Protocol::Messages && tool_results(item) {
            if let Some(previous) = records.last_mut().filter(|item| tool_results(item)) {
              previous["content"]
                .as_array_mut()
                .unwrap()
                .extend(item["content"].as_array().unwrap().iter().cloned());
              continue;
            }
          }
          records.push(item.clone());
        }
      }
      let key = if protocol == Protocol::Responses {
        "instructions"
      } else {
        "system"
      };
      if let Some(text) = converted.get(key).and_then(Value::as_str) {
        instructions.push(text.to_owned());
      }
    }
    let key = if protocol == Protocol::Responses {
      "input"
    } else {
      "messages"
    };
    body[key] = Value::Array(records);
    if !instructions.is_empty() {
      let key = if protocol == Protocol::Responses {
        "instructions"
      } else {
        "system"
      };
      body[key] = json!(instructions.join("\n\n"));
    }
    body["stream"] = json!(request.stream);
    if protocol == Protocol::Responses {
      body["store"] = json!(false);
      body["include"] = json!(["reasoning.encrypted_content"]);
    }
  }
  tokn_accounts::native::transform_body(
    driver(config).map_err(|_| invalid())?,
    protocol.endpoint(),
    body,
    None,
  )
  .map_err(|_| {
    Failure::new(
      "invalid_request",
      "Could not apply the provider's request requirements.",
    )
  })
}

fn tool_results(message: &Value) -> bool {
  message["role"] == "user"
    && message
      .get("content")
      .and_then(Value::as_array)
      .is_some_and(|blocks| {
        !blocks.is_empty() && blocks.iter().all(|block| block["type"] == "tool_result")
      })
}

pub(super) fn project(protocol: Protocol, output: &[Value]) -> Result<Projection, Failure> {
  let mut result = Projection {
    content: String::new(),
    calls: Vec::new(),
    reasoning: Vec::new(),
    refusal: false,
  };
  match protocol {
    Protocol::Responses => {
      for item in output {
        let kind = item
          .get("type")
          .and_then(Value::as_str)
          .ok_or_else(invalid)?;
        if item
          .get("status")
          .is_some_and(|status| !status.is_null() && status.as_str() != Some("completed"))
        {
          return Err(invalid());
        }
        match kind {
          "message" => {
            if item["role"] != "assistant" {
              return Err(invalid());
            }
            for part in item["content"].as_array().ok_or_else(invalid)? {
              match part["type"].as_str() {
                Some("output_text" | "text") => result
                  .content
                  .push_str(part["text"].as_str().ok_or_else(invalid)?),
                Some("refusal") => {
                  result.refusal = true;
                  result
                    .content
                    .push_str(part["refusal"].as_str().ok_or_else(invalid)?);
                }
                _ => return Err(Failure::unsupported("output_content")),
              }
            }
          }
          "function_call" => result.calls.push(LlmToolCall {
            id: item["call_id"]
              .as_str()
              .filter(|id| validation::valid_call_id(id))
              .ok_or_else(invalid)?
              .into(),
            name: item["name"]
              .as_str()
              .filter(|name| validation::valid_tool_name(name))
              .ok_or_else(invalid)?
              .into(),
            arguments: item["arguments"].as_str().ok_or_else(invalid)?.into(),
          }),
          "reasoning" => {
            if let Some(summary) = item.get("summary").and_then(Value::as_array) {
              result.reasoning.extend(
                summary
                  .iter()
                  .filter_map(|part| part.get("text").and_then(Value::as_str))
                  .map(str::to_owned),
              );
            }
          }
          "custom_tool_call" => return Err(Failure::unsupported("tool_type")),
          // Other provider-owned output items remain intact in replay state.
          _ => {}
        }
      }
    }
    Protocol::Messages => {
      if output.len() != 1 || output[0]["role"] != "assistant" {
        return Err(invalid());
      }
      for block in output[0]["content"].as_array().ok_or_else(invalid)? {
        match block["type"].as_str() {
          Some("text") => result
            .content
            .push_str(block["text"].as_str().ok_or_else(invalid)?),
          Some("thinking") => result
            .reasoning
            .push(block["thinking"].as_str().ok_or_else(invalid)?.into()),
          Some("redacted_thinking") => {}
          Some("tool_use") => result.calls.push(LlmToolCall {
            id: block["id"]
              .as_str()
              .filter(|id| validation::valid_call_id(id))
              .ok_or_else(invalid)?
              .into(),
            name: block["name"]
              .as_str()
              .filter(|name| validation::valid_tool_name(name))
              .ok_or_else(invalid)?
              .into(),
            arguments: serde_json::to_string(block.get("input").ok_or_else(invalid)?)
              .map_err(|_| invalid())?,
          }),
          _ => return Err(Failure::unsupported("output_content")),
        }
      }
    }
    Protocol::ChatCompletions => return Err(invalid()),
  }
  Ok(result)
}

use super::{Failure, LlmConfig, LlmRequest, LlmToolChoice, MAX_INPUT_BYTES, MAX_TOOLS};
use reqwest::Url;
use std::collections::HashSet;

pub fn validate_config(config: &LlmConfig) -> Result<(), String> {
  if config.model_id.trim().is_empty()
    || config.model_id.len() > 256
    || config.model_id.chars().any(char::is_control)
  {
    return Err("Enter a valid model ID.".into());
  }
  if !valid_id(&config.connection_id) {
    return Err("The connection identity is invalid.".into());
  }
  if config.base_url != "demo" {
    completion_url(config)?;
  }
  Ok(())
}

pub(super) fn completion_url(config: &LlmConfig) -> Result<Url, String> {
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
  url.set_path(&format!(
    "{}/chat/completions",
    url.path().trim_end_matches('/')
  ));
  Ok(url)
}

pub(super) fn valid_id(value: &str) -> bool {
  !value.is_empty()
    && value.len() <= 128
    && value
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(super) fn valid_tool_name(value: &str) -> bool {
  value.len() <= 64 && valid_id(value)
}

pub(super) fn valid_call_id(value: &str) -> bool {
  !value.is_empty() && value.len() <= 256 && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn invalid(message: &str) -> Failure {
  Failure::new("invalid_request", message)
}

pub(super) fn request(request: &LlmRequest) -> Result<(), Failure> {
  if !valid_id(&request.request_id) {
    return Err(invalid("The LLM request ID is invalid."));
  }
  if request.messages.is_empty() || request.messages.len() > 256 {
    return Err(invalid(
      "An LLM request must contain between 1 and 256 messages.",
    ));
  }
  if request
    .temperature
    .is_some_and(|value| !value.is_finite() || !(0.0..=2.0).contains(&value))
  {
    return Err(invalid("Temperature must be between 0 and 2."));
  }
  // Include tool schemas, arguments, results, and metadata, not only chat text.
  if serde_json::to_vec(request)
    .map_err(|_| invalid("The LLM request is not valid JSON."))?
    .len()
    > MAX_INPUT_BYTES
  {
    return Err(invalid("The LLM request exceeds the context size limit."));
  }
  if request.tools.len() > MAX_TOOLS {
    return Err(invalid("Too many tool definitions."));
  }
  let mut names = HashSet::new();
  for tool in &request.tools {
    if !valid_tool_name(&tool.name)
      || !names.insert(tool.name.as_str())
      || !tool.parameters.is_object()
    {
      return Err(invalid(
        "Tool definitions require unique valid names and JSON object parameters.",
      ));
    }
    validate_json_depth(&tool.parameters, 0)?;
  }
  match &request.tool_choice {
    Some(LlmToolChoice::Mode(mode)) if !matches!(mode.as_str(), "auto" | "none" | "required") => {
      return Err(invalid("The tool choice is invalid."))
    }
    Some(LlmToolChoice::Mode(mode)) if mode == "required" && request.tools.is_empty() => {
      return Err(invalid("Required tool choice needs a tool definition."))
    }
    Some(LlmToolChoice::Named { name }) if !names.contains(name.as_str()) => {
      return Err(invalid("The selected tool is not defined."))
    }
    _ => {}
  }
  let mut calls = HashSet::new();
  let mut pending = HashSet::new();
  let mut meaningful = false;
  for message in &request.messages {
    if !matches!(
      message.role.as_str(),
      "system" | "user" | "assistant" | "tool"
    ) {
      return Err(invalid("The LLM message role is unsupported."));
    }
    meaningful |=
      !message.content.is_empty() || !message.tool_calls.is_empty() || message.role == "tool";
    if message.role == "tool" {
      let Some(id) = message.tool_call_id.as_deref() else {
        return Err(invalid("A tool result requires its tool call ID."));
      };
      if !pending.remove(id) {
        return Err(invalid(
          "A tool result does not match an outstanding assistant tool call.",
        ));
      }
    } else if !pending.is_empty() {
      return Err(invalid(
        "Provide every tool result before the next conversation message.",
      ));
    } else if message.tool_call_id.is_some() {
      return Err(invalid("Only tool results can carry a tool call ID."));
    }
    if message.role != "assistant"
      && (!message.tool_calls.is_empty() || message.continuation_id.is_some())
    {
      return Err(invalid(
        "Only assistant messages can carry tool calls or continuation handles.",
      ));
    }
    if message.tool_calls.len() > MAX_TOOLS {
      return Err(invalid("Too many assistant tool calls."));
    }
    for call in &message.tool_calls {
      if !valid_call_id(&call.id) || !valid_tool_name(&call.name) || !calls.insert(call.id.as_str())
      {
        return Err(invalid(
          "Assistant tool calls require unique valid IDs and valid names.",
        ));
      }
      pending.insert(call.id.as_str());
    }
    if message
      .continuation_id
      .as_deref()
      .is_some_and(|id| !valid_id(id))
    {
      return Err(invalid("The continuation handle is invalid."));
    }
  }
  if !pending.is_empty() {
    return Err(invalid(
      "Provide the outstanding tool results before generating another reply.",
    ));
  }
  if !meaningful {
    return Err(invalid("The LLM request is empty."));
  }
  Ok(())
}

fn validate_json_depth(value: &serde_json::Value, depth: usize) -> Result<(), Failure> {
  if depth > 64 {
    return Err(invalid("The tool schema is nested too deeply."));
  }
  match value {
    serde_json::Value::Array(items) => {
      for item in items {
        validate_json_depth(item, depth + 1)?;
      }
    }
    serde_json::Value::Object(items) => {
      for item in items.values() {
        validate_json_depth(item, depth + 1)?;
      }
    }
    _ => {}
  }
  Ok(())
}

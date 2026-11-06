use super::{
  media, Failure, LlmConfig, LlmMediaOutput, LlmRequest, LlmTarget, LlmToolCall, LlmToolChoice,
  MAX_INPUT_BYTES, MAX_TOOLS,
};
use reqwest::Url;
use std::collections::HashSet;
use tokn_sdk::policy::{CanonicalAuthority, InvalidAuthority, InvalidHost};

impl LlmTarget {
  /// Validate the exact snapshot before reading credentials or contacting Tokn.
  pub fn validate_for(&self, config: &LlmConfig) -> Result<(), String> {
    if config.provider_id == "demo"
      || config.base_url == "demo"
      || self.connection_id != config.connection_id
      || self.model_id != config.model_id
      || self.model_id != self.model_id.trim()
    {
      return Err("Choose a saved provider connection and a valid model.".into());
    }
    validate_config(config)
  }
}

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
    // Settings and generation must admit the same endpoint. Compile only the
    // in-memory rules here; this reads no credentials and performs no I/O.
    super::tokn_config::execution_rules(config)?;
  }
  Ok(())
}

pub(super) fn base_url(config: &LlmConfig) -> Result<Url, String> {
  if config.base_url.len() > 2048 {
    return Err("The provider URL is too long.".into());
  }
  let url = Url::parse(&config.base_url).map_err(|_| "Enter a valid provider URL.".to_owned())?;
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
  CanonicalAuthority::parse(&url[url::Position::BeforeHost..url::Position::AfterPort])
    .map_err(authority_error)?;
  Ok(url)
}

#[cfg(test)]
pub(super) fn completion_url(config: &LlmConfig) -> Result<Url, String> {
  let mut url = base_url(config)?;
  url.set_path(&format!(
    "{}/chat/completions",
    url.path().trim_end_matches('/')
  ));
  Ok(url)
}

fn authority_error(error: InvalidAuthority) -> String {
  match error {
    InvalidAuthority::InvalidPort => "Provider URL ports must be between 1 and 65535.",
    InvalidAuthority::Host(InvalidHost::TrailingDot) => {
      "Provider hostnames must not end with a dot."
    }
    InvalidAuthority::Host(InvalidHost::InvalidDnsLabel) => {
      "Provider hostnames must use DNS labels of 1 to 63 letters, digits, or interior hyphens."
    }
    InvalidAuthority::Host(InvalidHost::TooLong) => {
      "Provider hostnames must be at most 253 characters."
    }
    _ => "Use a valid DNS hostname or IP address for the provider.",
  }
  .into()
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
  if request
    .max_output_tokens
    .is_some_and(|limit| !(1..=131_072).contains(&limit))
  {
    return Err(invalid("Max output tokens must be between 1 and 131072."));
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
    validate_json_depth(&tool.parameters, 3)?;
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
  match &request.output {
    Some(LlmMediaOutput::Audio { voice, .. }) if !valid_id(voice) => {
      return Err(invalid("A generated audio request needs a valid voice ID."));
    }
    Some(LlmMediaOutput::Image { .. })
      if request
        .tool_choice
        .as_ref()
        .is_some_and(|choice| !matches!(choice, LlmToolChoice::Mode(mode) if mode == "auto")) =>
    {
      return Err(invalid(
        "Image generation accepts only automatic tool choice.",
      ));
    }
    _ => {}
  }
  let mut calls = HashSet::new();
  let mut pending = HashSet::new();
  let mut meaningful = false;
  for message in &request.messages {
    let object = message
      .as_object()
      .ok_or_else(|| invalid("Messages must be JSON objects."))?;
    let tool_calls = message_fields(object)?;
    let role = object["role"]
      .as_str()
      .expect("message_fields validates the role");
    let has_content = match media::parts(message) {
      Some(parts) => media::validate_parts(parts)?,
      None => object
        .get("content")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|text| !text.is_empty()),
    };
    meaningful |= has_content || !tool_calls.is_empty() || role == "tool";
    if role == "tool" {
      let id = object
        .get("tool_call_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| valid_call_id(id))
        .ok_or_else(|| invalid("A tool result requires its tool call ID."))?;
      if !pending.remove(id) {
        return Err(invalid(
          "A tool result does not match an outstanding assistant tool call.",
        ));
      }
    } else if !pending.is_empty() {
      return Err(invalid(
        "Provide every tool result before the next conversation message.",
      ));
    }
    for call in tool_calls {
      if !calls.insert(call.id.clone()) {
        return Err(invalid("Assistant tool calls require unique IDs."));
      }
      pending.insert(call.id);
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

/// Shared message shape checks keep successful outputs valid as future input.
/// Unknown provider metadata remains untouched.
pub(super) fn message_fields(
  object: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<LlmToolCall>, Failure> {
  for value in object.values() {
    // Request object -> messages array -> message object -> field value.
    // Match the SDK's whole-request depth limit so returned messages replay.
    validate_json_depth(value, 3)?;
  }
  let role = object
    .get("role")
    .and_then(serde_json::Value::as_str)
    .filter(|role| matches!(*role, "system" | "user" | "assistant" | "tool"))
    .ok_or_else(|| invalid("The LLM message role is unsupported."))?;
  if let Some(content) = object.get("content") {
    if !(content.is_null() || content.is_string() || (role == "user" && content.is_array())) {
      return Err(invalid(
        "Message content must be a string or null, or a list of parts in a user message.",
      ));
    }
  }
  if role == "tool" {
    if object
      .get("tool_call_id")
      .and_then(serde_json::Value::as_str)
      .is_none_or(|id| !valid_call_id(id))
    {
      return Err(invalid("A tool result requires its tool call ID."));
    }
  } else if object
    .get("tool_call_id")
    .is_some_and(|value| !value.is_null())
  {
    return Err(invalid("Only tool results can carry a tool call ID."));
  }
  if object
    .get("function_call")
    .is_some_and(|value| !value.is_null())
  {
    return Err(Failure::unsupported("legacy_function_call"));
  }
  let calls = match object.get("tool_calls") {
    None | Some(serde_json::Value::Null) => &[][..],
    Some(serde_json::Value::Array(calls)) if role == "assistant" => calls.as_slice(),
    _ => {
      return Err(invalid(
        "Only assistant messages can carry an array of tool calls.",
      ))
    }
  };
  if calls.len() > MAX_TOOLS {
    return Err(invalid("Too many assistant tool calls."));
  }
  calls.iter().map(tool_call).collect()
}

/// Validate understood fields without projecting away provider-owned fields.
fn tool_call(value: &serde_json::Value) -> Result<LlmToolCall, Failure> {
  let fields = value
    .as_object()
    .ok_or_else(|| invalid("A tool call must be an object."))?;
  if fields.get("type").and_then(serde_json::Value::as_str) != Some("function") {
    return Err(Failure::unsupported("tool_type"));
  }
  let id = fields
    .get("id")
    .and_then(serde_json::Value::as_str)
    .filter(|id| valid_call_id(id))
    .ok_or_else(|| invalid("A tool call requires a valid ID."))?;
  let function = fields
    .get("function")
    .and_then(serde_json::Value::as_object)
    .ok_or_else(|| invalid("A tool call requires a function."))?;
  let name = function
    .get("name")
    .and_then(serde_json::Value::as_str)
    .filter(|name| valid_tool_name(name))
    .ok_or_else(|| invalid("A tool call requires a valid function name."))?;
  let arguments = function
    .get("arguments")
    .and_then(serde_json::Value::as_str)
    .ok_or_else(|| invalid("Tool arguments must be JSON text."))?;
  Ok(LlmToolCall {
    id: id.into(),
    name: name.into(),
    arguments: arguments.into(),
  })
}

fn validate_json_depth(value: &serde_json::Value, depth: usize) -> Result<(), Failure> {
  if depth >= 64 && (value.is_array() || value.is_object()) {
    return Err(invalid("The request JSON is nested too deeply."));
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

#[cfg(test)]
mod tests {
  use super::*;

  fn config(base_url: &str) -> LlmConfig {
    LlmConfig {
      provider_id: "custom".into(),
      base_url: base_url.into(),
      model_id: "namespace/custom-model".into(),
      connection_id: "test_connection".into(),
    }
  }

  #[test]
  fn settings_reject_incompatible_authorities_with_sanitized_reasons() {
    let label_error =
      "Provider hostnames must use DNS labels of 1 to 63 letters, digits, or interior hyphens.";
    let mut cases = vec![
      (
        "https://example.invalid./v1".to_owned(),
        "Provider hostnames must not end with a dot.",
      ),
      (
        "https://example.invalid:0/v1".to_owned(),
        "Provider URL ports must be between 1 and 65535.",
      ),
      (
        "http://localhost:0/v1".to_owned(),
        "Provider URL ports must be between 1 and 65535.",
      ),
      (
        "http://[::1]:0/v1".to_owned(),
        "Provider URL ports must be between 1 and 65535.",
      ),
    ];
    for host in [
      "bad_name.example".to_owned(),
      "-edge.example".to_owned(),
      "edge-.example".to_owned(),
      "empty..example".to_owned(),
      format!("{}.example", "x".repeat(64)),
    ] {
      cases.push((format!("https://{host}/v1"), label_error));
    }
    cases.push((
      format!("https://{}/v1", vec!["x".repeat(63); 4].join(".")),
      "Provider hostnames must be at most 253 characters.",
    ));
    for (url, expected) in cases {
      // These inputs pass the old URL parser, but cannot be used by Tokn.
      assert!(
        Url::parse(&url).is_ok(),
        "fixture does not exercise a compatibility gap"
      );
      assert_eq!(validate_config(&config(&url)).unwrap_err(), expected);
      assert_eq!(
        super::super::tokn_config::execution_rules(&config(&url)).unwrap_err(),
        expected
      );
    }
  }

  #[test]
  fn settings_preserve_normalized_urls_localhost_and_model_validation() {
    for url in [
      "demo",
      "https://EXAMPLE.invalid:443/tenant/v1///",
      "https://xn--bcher-kva.example/v1",
      "https://bücher.example/v1",
      "https://example.invalid/proxy%2Ftenant/v1",
      "http://localhost:8000/v1",
      "http://127.1:8000/v1",
      "http://[::1]:8000/v1",
    ] {
      assert!(validate_config(&config(url)).is_ok(), "rejected {url}");
    }
    let mut invalid = config("https://example.invalid/v1");
    invalid.model_id.clear();
    assert_eq!(
      validate_config(&invalid).unwrap_err(),
      "Enter a valid model ID."
    );
    invalid.model_id = "model".into();
    invalid.connection_id = "invalid connection".into();
    assert_eq!(
      validate_config(&invalid).unwrap_err(),
      "The connection identity is invalid."
    );
  }
}

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
  #[serde(default = "default_provider")]
  pub provider_id: String,
  pub base_url: String,
  pub model_id: String,
  /// Host-generated public identity, independent of endpoint and credentials.
  #[serde(default = "new_connection_id")]
  pub connection_id: String,
}

fn default_provider() -> String {
  "custom".into()
}

fn new_connection_id() -> String {
  Uuid::new_v4().to_string()
}

impl Default for LlmConfig {
  fn default() -> Self {
    Self {
      provider_id: "demo".into(),
      base_url: "demo".into(),
      model_id: "local-demo".into(),
      connection_id: new_connection_id(),
    }
  }
}

impl LlmConfig {
  pub fn capabilities(&self) -> LlmCapabilities {
    // An arbitrary compatible endpoint does not advertise model capabilities.
    // Do not infer them from its URL, model name, or lack of reasoning output.
    let state = if self.base_url == "demo" {
      CapabilityState::Unsupported
    } else {
      CapabilityState::Unknown
    };
    let protocol = (self.base_url != "demo")
      .then(|| super::wire::protocol(self).ok())
      .flatten();
    let output = |supports| {
      if supports {
        state
      } else {
        CapabilityState::Unsupported
      }
    };
    LlmCapabilities {
      tools: state,
      required_tool_choice: state,
      exposed_reasoning: state,
      image_input: state,
      pdf_input: state,
      audio_input: output(protocol.is_some_and(|protocol| protocol.supports_audio_input())),
      image_output: output(protocol.is_some_and(|protocol| protocol.supports_image_output(self))),
      audio_output: output(protocol.is_some_and(|protocol| protocol.supports_audio_output())),
    }
  }

  pub fn provenance(&self) -> LlmProvenance {
    LlmProvenance {
      connection_id: self.connection_id.clone(),
      requested_model_id: self.model_id.clone(),
      model_id: (self.base_url == "demo").then(|| "local-demo".into()),
    }
  }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
  Supported,
  Unsupported,
  Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LlmCapabilities {
  pub tools: CapabilityState,
  pub required_tool_choice: CapabilityState,
  pub exposed_reasoning: CapabilityState,
  pub image_input: CapabilityState,
  pub pdf_input: CapabilityState,
  pub audio_input: CapabilityState,
  pub image_output: CapabilityState,
  pub audio_output: CapabilityState,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LlmImageFormat {
  #[default]
  Png,
  Jpeg,
  Webp,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LlmAudioFormat {
  #[default]
  Wav,
  Mp3,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LlmMediaOutput {
  Image {
    #[serde(default)]
    format: LlmImageFormat,
  },
  Audio {
    voice: String,
    #[serde(default)]
    format: LlmAudioFormat,
  },
}

impl LlmMediaOutput {
  pub(super) fn capability(&self) -> &'static str {
    match self {
      Self::Image { .. } => "image_output",
      Self::Audio { .. } => "audio_output",
    }
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LlmMediaFile {
  #[serde(rename = "type")]
  pub kind: LlmMediaKind,
  pub path: String,
  pub mime_type: String,
  pub size_bytes: usize,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub transcript: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmMediaKind {
  Image,
  Audio,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LlmToolCall {
  pub id: String,
  pub name: String,
  /// JSON text, kept intact for the applet to validate against its tool schema.
  pub arguments: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmToolDefinition {
  pub name: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub description: Option<String>,
  pub parameters: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum LlmToolChoice {
  Mode(String),
  Named { name: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LlmTarget {
  pub connection_id: String,
  pub model_id: String,
}

impl LlmTarget {
  pub fn provenance(&self) -> LlmProvenance {
    LlmProvenance {
      connection_id: self.connection_id.clone(),
      requested_model_id: self.model_id.clone(),
      model_id: None,
    }
  }
}

/// Only an omitted target selects the active default. A present null must fail.
pub fn deserialize_target<'de, D: serde::Deserializer<'de>>(
  value: D,
) -> Result<Option<LlmTarget>, D::Error> {
  LlmTarget::deserialize(value).map(Some)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmRequest {
  pub request_id: String,
  #[serde(
    default,
    deserialize_with = "deserialize_target",
    skip_serializing_if = "Option::is_none"
  )]
  pub target: Option<LlmTarget>,
  /// Complete messages, including applet-owned metadata and private file references.
  pub messages: Vec<Value>,
  #[serde(default = "default_stream")]
  pub stream: bool,
  #[serde(default)]
  pub max_output_tokens: Option<u32>,
  #[serde(default)]
  pub temperature: Option<f64>,
  #[serde(default)]
  pub tools: Vec<LlmToolDefinition>,
  #[serde(default)]
  pub tool_choice: Option<LlmToolChoice>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub output: Option<LlmMediaOutput>,
}

fn default_stream() -> bool {
  true
}

impl Default for LlmRequest {
  fn default() -> Self {
    Self {
      request_id: String::new(),
      target: None,
      messages: Vec::new(),
      stream: true,
      max_output_tokens: None,
      temperature: None,
      tools: Vec::new(),
      tool_choice: None,
      output: None,
    }
  }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LlmStatus {
  Completed,
  ToolCalls,
  Cancelled,
  Truncated,
  Unsupported,
  Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct LlmProvenance {
  pub connection_id: String,
  pub requested_model_id: String,
  /// Actual provider-reported model. Null when unavailable; never inferred.
  pub model_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LlmFailure {
  pub code: String,
  pub message: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub capability: Option<String>,
  pub retryable: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LlmResult {
  pub request_id: String,
  pub status: LlmStatus,
  pub content: String,
  pub tool_calls: Vec<LlmToolCall>,
  pub media: Vec<LlmMediaFile>,
  /// Complete assistant message, present only for a successful generation.
  pub message: Option<Value>,
  pub finish_reason: Option<String>,
  pub provenance: LlmProvenance,
  pub error: Option<LlmFailure>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LlmEvent {
  pub request_id: String,
  pub kind: String,
  pub text: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub provenance: Option<LlmProvenance>,
}

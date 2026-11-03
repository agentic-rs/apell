use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// UTF-8 JSON byte limit for one backend ABI message.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PENDING: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HostError {
  pub message: String,
  #[serde(default, skip_serializing_if = "String::is_empty")]
  pub partial_content: String,
}

impl HostError {
  pub fn new(message: impl fmt::Display) -> Self {
    Self {
      message: message.to_string(),
      partial_content: String::new(),
    }
  }
}

impl fmt::Display for HostError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.message)
  }
}

impl std::error::Error for HostError {}

// An explicit null is a valid successful result. These adjacent fields use a
// flattened untagged enum so null does not become a missing Option<Value>.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Outcome {
  Success { result: Value },
  Failure { error: HostError },
}

impl From<Result<Value, HostError>> for Outcome {
  fn from(value: Result<Value, HostError>) -> Self {
    match value {
      Ok(result) => Self::Success { result },
      Err(error) => Self::Failure { error },
    }
  }
}

impl From<Outcome> for Result<Value, HostError> {
  fn from(value: Outcome) -> Self {
    match value {
      Outcome::Success { result } => Ok(result),
      Outcome::Failure { error } => Err(error),
    }
  }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Inbound {
  Invoke {
    request_id: String,
    command: String,
    params: Value,
  },
  HostResult {
    request_id: String,
    #[serde(flatten)]
    outcome: Outcome,
  },
  HostEvent {
    request_id: String,
    event: Value,
  },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outbound {
  HostCall {
    request_id: String,
    method: String,
    params: Value,
  },
  Reply {
    request_id: String,
    #[serde(flatten)]
    outcome: Outcome,
  },
  Event {
    event: Value,
  },
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn successful_null_and_failure_are_distinct_in_both_directions() {
    for (kind, field) in [
      ("host_result", "result"),
      ("reply", "result"),
      ("host_result", "error"),
      ("reply", "error"),
    ] {
      let mut wire = json!({"kind":kind,"request_id":"fixture"});
      wire[field] = if field == "error" {
        json!({"message":"interrupted","partial_content":"partial"})
      } else {
        Value::Null
      };
      let encoded = if kind == "reply" {
        serde_json::to_value(serde_json::from_value::<Outbound>(wire.clone()).unwrap()).unwrap()
      } else {
        serde_json::to_value(serde_json::from_value::<Inbound>(wire.clone()).unwrap()).unwrap()
      };
      assert_eq!(wire, encoded);
    }
  }
}

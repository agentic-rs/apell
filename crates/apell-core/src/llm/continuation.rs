use super::{provider::Output, Failure, LlmConfig, LlmMessage, LlmToolCall, MAX_INPUT_BYTES};
use serde_json::{json, Map, Value};
use std::{
  collections::HashMap,
  time::{Duration, Instant},
};
use uuid::Uuid;

const TTL: Duration = Duration::from_secs(30 * 60);
const MAX_ENTRIES: usize = 64;
const MAX_SESSION_ENTRIES: usize = 16;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;

struct Entry {
  session_id: String,
  configuration: String,
  assistant: String,
  opaque: Map<String, Value>,
  size: usize,
  created: Instant,
}

#[derive(Default)]
pub(super) struct ContinuationCache {
  entries: HashMap<String, Entry>,
}

fn configuration(config: &LlmConfig) -> String {
  crate::bundles::digest(
    serde_json::to_string(&(
      config.connection_id.as_str(),
      config.base_url.as_str(),
      config.model_id.as_str(),
    ))
    .expect("strings serialize")
    .as_bytes(),
  )
}

fn assistant(content: &str, calls: &[LlmToolCall]) -> String {
  crate::bundles::digest(
    serde_json::to_string(&(content, calls))
      .expect("assistant strings serialize")
      .as_bytes(),
  )
}

fn invalid() -> Failure {
  Failure::new("invalid_continuation", "The continuation is expired, belongs to another session or connection, or its assistant message changed. Start a fresh context without this continuation.")
}

impl ContinuationCache {
  pub fn prune(&mut self) {
    self
      .entries
      .retain(|_, entry| entry.created.elapsed() < TTL);
  }

  pub fn close_session(&mut self, session_id: &str) {
    self
      .entries
      .retain(|_, entry| entry.session_id != session_id);
  }

  pub fn insert(
    &mut self,
    session_id: &str,
    config: &LlmConfig,
    output: &Output,
  ) -> Result<Option<String>, Failure> {
    if output.opaque.is_empty() {
      return Ok(None);
    }
    self.prune();
    let size = serde_json::to_vec(&output.opaque)
      .map_err(|_| Failure::new("invalid_response", "The provider continuation is invalid."))?
      .len();
    if size > MAX_INPUT_BYTES {
      return Err(Failure::new(
        "continuation_limit",
        "The provider continuation exceeds the context size limit.",
      ));
    }
    while self.entries.len() >= MAX_ENTRIES
      || self.entries.values().map(|entry| entry.size).sum::<usize>() + size > MAX_CACHE_BYTES
      || self
        .entries
        .values()
        .filter(|entry| entry.session_id == session_id)
        .count()
        >= MAX_SESSION_ENTRIES
    {
      let session_full = self
        .entries
        .values()
        .filter(|entry| entry.session_id == session_id)
        .count()
        >= MAX_SESSION_ENTRIES;
      let oldest = self
        .entries
        .iter()
        .filter(|(_, entry)| !session_full || entry.session_id == session_id)
        .min_by_key(|(_, entry)| entry.created)
        .map(|(id, _)| id.clone());
      if let Some(id) = oldest {
        self.entries.remove(&id);
      } else {
        break;
      }
    }
    let id = Uuid::new_v4().to_string();
    self.entries.insert(
      id.clone(),
      Entry {
        session_id: session_id.into(),
        configuration: configuration(config),
        assistant: assistant(&output.content, &output.tool_calls),
        opaque: output.opaque.clone(),
        size,
        created: Instant::now(),
      },
    );
    Ok(Some(id))
  }
}

pub(super) fn messages(
  cache: &mut ContinuationCache,
  session_id: &str,
  config: &LlmConfig,
  messages: &[LlmMessage],
) -> Result<Vec<Value>, Failure> {
  cache.prune();
  let binding = configuration(config);
  let mut result = Vec::with_capacity(messages.len());
  for message in messages {
    let mut value = json!({"role": message.role, "content": message.content});
    if !message.tool_calls.is_empty() {
      value["tool_calls"] = Value::Array(message.tool_calls.iter().map(|call| json!({"id": call.id, "type": "function", "function": {"name": call.name, "arguments": call.arguments}})).collect());
    }
    if let Some(id) = &message.tool_call_id {
      value["tool_call_id"] = json!(id);
    }
    if let Some(id) = &message.continuation_id {
      let entry = cache.entries.get(id).ok_or_else(invalid)?;
      if entry.session_id != session_id
        || entry.configuration != binding
        || entry.assistant != assistant(&message.content, &message.tool_calls)
      {
        return Err(invalid());
      }
      value
        .as_object_mut()
        .expect("message is an object")
        .extend(entry.opaque.clone());
    }
    result.push(value);
  }
  if serde_json::to_vec(&result)
    .map_err(|_| Failure::new("invalid_request", "The context is not valid JSON."))?
    .len()
    > MAX_INPUT_BYTES
  {
    return Err(Failure::new(
      "context_limit",
      "The context including private continuation data exceeds the size limit.",
    ));
  }
  Ok(result)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cache_expiry_bounds_and_session_close_remove_private_state() {
    let config = LlmConfig::default();
    let mut cache = ContinuationCache::default();
    let mut output = Output::new("test", config.provenance());
    output.content = "answer".into();
    output
      .opaque
      .insert("reasoning_content".into(), json!("private"));
    let first = cache.insert("session", &config, &output).unwrap().unwrap();
    for _ in 0..MAX_SESSION_ENTRIES {
      cache.insert("session", &config, &output).unwrap();
    }
    assert_eq!(cache.entries.len(), MAX_SESSION_ENTRIES);
    assert!(!cache.entries.contains_key(&first));
    let other = cache.insert("other", &config, &output).unwrap().unwrap();
    cache.entries.get_mut(&other).unwrap().created = Instant::now() - TTL;
    cache.prune();
    assert!(!cache.entries.contains_key(&other));
    cache.close_session("session");
    assert!(cache.entries.is_empty());
  }
}

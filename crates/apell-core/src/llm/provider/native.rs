use super::*;
use wire::Protocol;

pub(super) fn take_response_images(response: &mut Value) -> Vec<Value> {
  let mut images = Vec::new();
  if let Some(items) = response.get_mut("output").and_then(Value::as_array_mut) {
    items.retain_mut(|item| {
      if item["type"] != "image_generation_call" {
        return true;
      }
      images.push(std::mem::take(item));
      false
    });
  }
  images
}

#[derive(Default)]
pub(super) struct NativeOutput {
  pub terminal: bool,
  reasoning_emitted: bool,
  message: Option<Value>,
  blocks: BTreeMap<usize, Value>,
  arguments: BTreeMap<usize, String>,
  closed_blocks: HashSet<usize>,
}

impl Output {
  fn native_reasoning(&mut self, text: &str, emit: &impl Fn(LlmEvent)) -> Result<(), Failure> {
    self.charge(text.len())?;
    if !text.is_empty() {
      self.native.reasoning_emitted = true;
      emit(event(&self.request_id, "reasoning_delta", text));
    }
    Ok(())
  }

  fn native_message(
    &mut self,
    config: &LlmConfig,
    protocol: Protocol,
    native: Vec<Value>,
    emit: &impl Fn(LlmEvent),
  ) -> Result<(), Failure> {
    if serde_json::to_vec(&native)
      .map_err(|_| invalid_response())?
      .len()
      > MAX_OUTPUT_BYTES
    {
      self.native_partial(protocol, &native, emit)?;
      return Err(Failure::new(
        "response_limit",
        "The native model output exceeded the size limit.",
      ));
    }
    let projection = match wire::project(protocol, &native) {
      Ok(projection) => projection,
      Err(error) => {
        self.native_partial(protocol, &native, emit)?;
        return Err(error);
      }
    };
    if !projection.content.starts_with(&self.content) {
      return Err(invalid_response());
    }
    let remaining = &projection.content[self.content.len()..];
    self.text(remaining, emit);
    self.refusal |= projection.refusal;
    if !self.native.reasoning_emitted {
      for text in &projection.reasoning {
        self.native_reasoning(text, emit)?;
      }
    }
    self.message = wire::message(config, protocol, native)?
      .as_object()
      .ok_or_else(invalid_response)?
      .clone();
    Ok(())
  }

  fn native_partial(
    &mut self,
    protocol: Protocol,
    output: &[Value],
    emit: &impl Fn(LlmEvent),
  ) -> Result<(), Failure> {
    let mut content = String::new();
    let mut overflow = false;
    'items: for item in output {
      if protocol == Protocol::Responses && item["type"] != "message" {
        continue;
      }
      if let Some(parts) = item.get("content").and_then(Value::as_array) {
        for part in parts {
          let field = match part["type"].as_str() {
            Some("text" | "output_text") => "text",
            Some("refusal") => "refusal",
            _ => continue,
          };
          if let Some(text) = part.get(field).and_then(Value::as_str) {
            let mut end = text.len().min(MAX_OUTPUT_BYTES - content.len());
            while !text.is_char_boundary(end) {
              end -= 1;
            }
            content.push_str(&text[..end]);
            if end != text.len() {
              overflow = true;
              break 'items;
            }
          }
        }
      }
    }
    if content.starts_with(&self.content) {
      self.text(&content[self.content.len()..], emit);
    }
    if overflow {
      return Err(Failure::new(
        "response_limit",
        "The native model output exceeded the size limit.",
      ));
    }
    Ok(())
  }

  pub(super) fn responses(
    &mut self,
    value: &Value,
    streaming: bool,
    config: &LlmConfig,
    emit: &impl Fn(LlmEvent),
  ) -> Result<(), Failure> {
    if !streaming {
      return self.responses_final(value, config, emit);
    }
    let kind = value
      .get("type")
      .and_then(Value::as_str)
      .ok_or_else(invalid_response)?;
    match kind {
      "response.created" | "response.in_progress" => self.metadata(&value["response"]),
      "response.output_text.delta" => {
        let text = value["delta"].as_str().ok_or_else(invalid_response)?;
        self.charge(text.len())?;
        self.text(text, emit);
        Ok(())
      }
      "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
        self.native_reasoning(value["delta"].as_str().ok_or_else(invalid_response)?, emit)
      }
      "response.refusal.delta" => {
        self.refusal = true;
        let text = value["delta"].as_str().ok_or_else(invalid_response)?;
        self.charge(text.len())?;
        self.text(text, emit);
        Ok(())
      }
      "response.completed" | "response.incomplete" | "response.cancelled" | "response.failed" => {
        self.responses_final(&value["response"], config, emit)
      }
      "error" => Err(provider_error(value, None)),
      // Final Responses output is authoritative and contains complete items.
      // Intermediate item/function/annotation deltas need no guessed merge.
      _ => Ok(()),
    }
  }

  fn responses_final(
    &mut self,
    response: &Value,
    config: &LlmConfig,
    emit: &impl Fn(LlmEvent),
  ) -> Result<(), Failure> {
    self.metadata(response)?;
    if let Some(output) = response.get("output").and_then(Value::as_array) {
      if response["status"] == "completed" {
        self.native_message(config, Protocol::Responses, output.clone(), emit)?;
      } else {
        self.native_partial(Protocol::Responses, output, emit)?;
      }
    }
    self.native.terminal = true;
    match response["status"].as_str() {
      Some("completed") => {
        if response.get("output").and_then(Value::as_array).is_none() {
          return Err(invalid_response());
        }
        self.finish_reason = Some(
          if self
            .message
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|calls| !calls.is_empty())
          {
            "tool_calls".into()
          } else {
            "stop".into()
          },
        );
        Ok(())
      }
      Some("incomplete") => {
        self.finish_reason = Some(
          match response
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
          {
            Some("max_output_tokens") => "length",
            Some("content_filter") => "content_filter",
            _ => "incomplete",
          }
          .into(),
        );
        Ok(())
      }
      Some("cancelled") => Err(Failure::new(
        "cancelled",
        "The provider cancelled the generation.",
      )),
      Some("failed") => Err(provider_error(response, None)),
      _ => Err(invalid_response()),
    }
  }

  pub(super) fn messages(
    &mut self,
    value: &Value,
    streaming: bool,
    config: &LlmConfig,
    emit: &impl Fn(LlmEvent),
  ) -> Result<(), Failure> {
    if !streaming {
      self.metadata(value)?;
      self.finish_reason =
        Some(message_finish(value.get("stop_reason").and_then(Value::as_str))?.into());
      if matches!(self.finish_reason.as_deref(), Some("stop" | "tool_calls")) {
        self.native_message(config, Protocol::Messages, vec![value.clone()], emit)?;
      } else {
        self.native_partial(Protocol::Messages, std::slice::from_ref(value), emit)?;
      }
      self.native.terminal = true;
      return Ok(());
    }
    match value
      .get("type")
      .and_then(Value::as_str)
      .ok_or_else(invalid_response)?
    {
      "message_start" => {
        if self.native.message.is_some() {
          return Err(invalid_response());
        }
        self.metadata(&value["message"])?;
        let message = value["message"].as_object().ok_or_else(invalid_response)?;
        self.charge(
          serde_json::to_vec(message)
            .map_err(|_| invalid_response())?
            .len(),
        )?;
        if message
          .get("content")
          .and_then(Value::as_array)
          .is_none_or(|items| !items.is_empty())
        {
          return Err(stream_metadata());
        }
        self.native.message = Some(value["message"].clone());
      }
      "content_block_start" => {
        let index = block_index(value)?;
        let block = value["content_block"]
          .as_object()
          .ok_or_else(invalid_response)?;
        if self.native.message.is_none() || self.native.blocks.contains_key(&index) {
          return Err(invalid_response());
        }
        self.charge(
          serde_json::to_vec(block)
            .map_err(|_| invalid_response())?
            .len(),
        )?;
        match block.get("type").and_then(Value::as_str) {
          Some("text") => self.text(
            block
              .get("text")
              .and_then(Value::as_str)
              .ok_or_else(invalid_response)?,
            emit,
          ),
          Some("thinking") => self.native_reasoning(
            block
              .get("thinking")
              .and_then(Value::as_str)
              .ok_or_else(invalid_response)?,
            emit,
          )?,
          Some("tool_use" | "redacted_thinking") => {}
          _ => return Err(Failure::unsupported("output_content")),
        }
        self
          .native
          .blocks
          .insert(index, value["content_block"].clone());
      }
      "content_block_delta" => {
        let index = block_index(value)?;
        if self.native.closed_blocks.contains(&index) {
          return Err(invalid_response());
        }
        let delta = value["delta"].as_object().ok_or_else(invalid_response)?;
        self.charge(
          serde_json::to_vec(delta)
            .map_err(|_| invalid_response())?
            .len(),
        )?;
        let block = self
          .native
          .blocks
          .get_mut(&index)
          .and_then(Value::as_object_mut)
          .ok_or_else(invalid_response)?;
        let (key, source) = match delta.get("type").and_then(Value::as_str) {
          Some("text_delta") if block.get("type").and_then(Value::as_str) == Some("text") => {
            ("text", "text")
          }
          Some("thinking_delta")
            if block.get("type").and_then(Value::as_str) == Some("thinking") =>
          {
            ("thinking", "thinking")
          }
          Some("signature_delta")
            if block.get("type").and_then(Value::as_str) == Some("thinking") =>
          {
            ("signature", "signature")
          }
          Some("input_json_delta")
            if block.get("type").and_then(Value::as_str) == Some("tool_use") =>
          {
            if delta
              .keys()
              .any(|key| !matches!(key.as_str(), "type" | "partial_json"))
            {
              return Err(stream_metadata());
            }
            let part = delta
              .get("partial_json")
              .and_then(Value::as_str)
              .ok_or_else(invalid_response)?;
            self
              .native
              .arguments
              .entry(index)
              .or_default()
              .push_str(part);
            return Ok(());
          }
          _ => return Err(stream_metadata()),
        };
        if delta.keys().any(|name| name != "type" && name != source) {
          return Err(stream_metadata());
        }
        let text = delta
          .get(source)
          .and_then(Value::as_str)
          .ok_or_else(invalid_response)?;
        append_string(block, key, &json!(text))?;
        if key == "text" {
          self.text(text, emit);
        }
        if key == "thinking" {
          self.native_reasoning(text, emit)?;
        }
      }
      "content_block_stop" => {
        let index = block_index(value)?;
        if !self.native.closed_blocks.insert(index) {
          return Err(invalid_response());
        }
        let block = self
          .native
          .blocks
          .get_mut(&index)
          .ok_or_else(invalid_response)?;
        if let Some(arguments) = self.native.arguments.remove(&index) {
          if block["input"]
            .as_object()
            .is_none_or(|input| !input.is_empty())
          {
            return Err(stream_metadata());
          }
          block["input"] = serde_json::from_str(&arguments).map_err(|_| invalid_response())?;
        }
      }
      "message_delta" => {
        let delta = value["delta"].as_object().ok_or_else(invalid_response)?;
        self.charge(
          serde_json::to_vec(value)
            .map_err(|_| invalid_response())?
            .len(),
        )?;
        if delta
          .keys()
          .any(|key| !matches!(key.as_str(), "stop_reason" | "stop_sequence"))
        {
          return Err(stream_metadata());
        }
        let message = self
          .native
          .message
          .as_mut()
          .and_then(Value::as_object_mut)
          .ok_or_else(invalid_response)?;
        message.extend(delta.clone());
        if let Some(usage) = value.get("usage") {
          let usage = usage.as_object().ok_or_else(invalid_response)?;
          let current = message
            .entry("usage")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(invalid_response)?;
          current.extend(usage.clone());
        }
      }
      "message_stop" => {
        let mut message = self.native.message.take().ok_or_else(invalid_response)?;
        message["content"] = Value::Array(self.native.blocks.values().cloned().collect());
        self.finish_reason =
          Some(message_finish(message.get("stop_reason").and_then(Value::as_str))?.into());
        if matches!(self.finish_reason.as_deref(), Some("stop" | "tool_calls")) {
          if !self.native.arguments.is_empty()
            || self.native.closed_blocks.len() != self.native.blocks.len()
          {
            return Err(invalid_response());
          }
          self.native_message(config, Protocol::Messages, vec![message], emit)?;
        } else {
          self.native_partial(Protocol::Messages, &[message], emit)?;
        }
        self.native.terminal = true;
      }
      "ping" => {}
      "error" => return Err(provider_error(value, None)),
      _ => return Err(stream_metadata()),
    }
    Ok(())
  }
}

fn block_index(value: &Value) -> Result<usize, Failure> {
  value
    .get("index")
    .and_then(Value::as_u64)
    .filter(|index| *index < 256)
    .map(|index| index as usize)
    .ok_or_else(invalid_response)
}

fn message_finish(reason: Option<&str>) -> Result<&str, Failure> {
  match reason {
    Some("end_turn" | "stop_sequence") => Ok("stop"),
    Some("tool_use") => Ok("tool_calls"),
    Some("max_tokens") => Ok("length"),
    Some("refusal") => Ok("content_filter"),
    Some(_) => Err(Failure::new(
      "unknown_finish_reason",
      "The provider returned an unsupported completion reason.",
    )),
    None => Err(Failure::new(
      "incomplete_response",
      "The provider did not finish the message.",
    )),
  }
}

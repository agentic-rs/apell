mod chat;
mod models;
mod selection;
mod state;
mod storage;

use apell_guest::HostError;
use serde_json::{json, Value};

apell_guest::export!(handle);

fn string<'a>(params: &'a Value, key: &str) -> Result<&'a str, HostError> {
  params[key]
    .as_str()
    .ok_or_else(|| HostError::new(format!("Missing {key}.")))
}

// The host invokes initialize once, before attaching the first view. The same
// instance serves later views, and a view can reconstruct everything it needs.
pub async fn handle(command: String, params: Value) -> Result<Value, HostError> {
  if command == "stop" {
    state::stop(string(&params, "request_id")?).await?;
    return Ok(Value::Null);
  }
  storage::serialized(async {
    match command.as_str() {
      "initialize" => {
        storage::initialize().await?;
        Ok(Value::Null)
      }
      "snapshot" => snapshot().await,
      "open_conversation" => {
        state::idle()?;
        let id = string(&params, "conversation_id")?;
        selection::load(id, None).await?;
        state::select(id);
        Ok(Value::Null)
      }
      "create_conversation" => {
        state::idle()?;
        let target = serde_json::from_value::<Option<selection::Target>>(params["target"].clone())
          .map_err(HostError::new)?;
        let id = selection::create(target.as_ref(), None).await?;
        state::select(&id);
        Ok(json!(id))
      }
      "choose_model" => {
        state::idle()?;
        let target = serde_json::from_value(params["target"].clone()).map_err(HostError::new)?;
        let id = selection::choose(string(&params, "conversation_id")?, target).await?;
        state::select(&id);
        Ok(json!(id))
      }
      "start_send" => {
        let id = chat::start(
          string(&params, "conversation_id")?,
          string(&params, "content")?,
        )
        .await?;
        Ok(json!({"request_id":id}))
      }
      _ => Err(HostError::new("Unknown Companion command.")),
    }
  })
  .await
}

async fn snapshot() -> Result<Value, HostError> {
  let mut conversations = storage::conversations().await?;
  let id = match state::conversation().or_else(|| {
    conversations
      .first()
      .map(|chat| chat.conversation_id.clone())
  }) {
    Some(id) => id,
    None => {
      let id = selection::create(None, None).await?;
      conversations = storage::conversations().await?;
      id
    }
  };
  if state::conversation().is_none() {
    state::select(&id);
  }
  let mut target = selection::load(&id, None).await?;
  let messages = storage::messages(&id).await?;
  if target.is_none() && messages.is_empty() {
    if let Ok(config) = apell_guest::call::<Value>("llm.config", json!({})).await {
      if config["mode"] == "provider" {
        let default: selection::Target = serde_json::from_value(config).map_err(HostError::new)?;
        selection::choose(&id, default.clone()).await?;
        target = Some(default);
      }
    }
  }
  let mut value = state::snapshot();
  value["conversations"] = json!(conversations);
  value["target"] = json!(target);
  value["messages"] = json!(messages);
  Ok(value)
}

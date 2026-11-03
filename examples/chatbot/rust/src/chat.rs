use crate::{models::title_from_message, selection, state, storage};
use apell_guest::HostError;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Completion {
  content: String,
  status: String,
  message: Option<Value>,
  provenance: Value,
  error: Option<CompletionError>,
}

#[derive(Deserialize)]
struct CompletionError {
  message: String,
}

fn is_assistant_message(message: &Value) -> bool {
  message.is_object() && message.get("role").and_then(Value::as_str) == Some("assistant")
}

fn history_message(row: &[Value]) -> Result<Value, HostError> {
  let role = storage::cell(row, 0)?;
  if let Some(encoded) = storage::optional_cell(row, 3)? {
    let message: Value = serde_json::from_str(&encoded)
      .map_err(|_| HostError::new("A stored assistant message cannot be read."))?;
    if role != "assistant" || !is_assistant_message(&message) {
      return Err(HostError::new("A stored assistant message is invalid."));
    }
    return Ok(message);
  }
  // User messages and replies written before schema v2 contain only text.
  Ok(json!({"role": role, "content": storage::cell(row, 1)?}))
}

async fn prepare(
  conversation_id: &str,
  content: &str,
  request_id: &str,
  assistant_id: &str,
) -> Result<(Vec<Value>, selection::Target), HostError> {
  let transaction = storage::begin().await?;
  let result = async {
    let model = selection::load(conversation_id, Some(&transaction)).await?
      .ok_or_else(|| HostError::new("Choose a provider and model before sending."))?;
    let rows = storage::query("SELECT role, content, status, provider_message FROM messages WHERE conversation_id = ?1 ORDER BY rowid LIMIT 1001", vec![storage::text(conversation_id)], Some(&transaction)).await?;
    if rows.len() >= 1000 { return Err(HostError::new("This conversation is full. Start a new conversation.")); }
    let mut messages = vec![json!({"role": "system", "content": "You are a helpful, thoughtful assistant. Answer clearly and directly."})];
    for row in &rows {
      match storage::cell(row, 2)?.as_str() {
        "pending" => return Err(HostError::new("This conversation already has a reply in progress.")),
        "complete" => messages.push(history_message(row)?),
        _ => {}
      }
    }
    messages.push(json!({"role": "user", "content": content}));
    let now = apell_guest::now();
    storage::execute("INSERT INTO messages (message_id, conversation_id, role, content, status, request_id, created_at) VALUES (?1, ?2, 'user', ?3, 'complete', ?4, ?5)", vec![storage::text(apell_guest::create_id()), storage::text(conversation_id), storage::text(content), storage::text(request_id), storage::text(&now)], Some(&transaction)).await?;
    storage::execute("INSERT INTO messages (message_id, conversation_id, role, content, status, request_id, created_at) VALUES (?1, ?2, 'assistant', '', 'pending', ?3, ?4)", vec![storage::text(assistant_id), storage::text(conversation_id), storage::text(request_id), storage::text(&now)], Some(&transaction)).await?;
    let title = if rows.is_empty() { title_from_message(content) } else { String::new() };
    // Pin legacy attribution in the same transaction as the new messages.
    storage::execute("UPDATE conversations SET updated_at = ?1, title = CASE WHEN ?2 = '' THEN title ELSE ?2 END, model_target = COALESCE(model_target, ?3) WHERE conversation_id = ?4",
      vec![storage::text(now), storage::text(title), storage::text(serde_json::to_string(&model).map_err(HostError::new)?), storage::text(conversation_id)], Some(&transaction)).await?;
    storage::commit(&transaction).await?;
    Ok((messages, model))
  }.await;
  if result.is_err() {
    storage::rollback(&transaction).await;
  }
  result
}

pub async fn start(conversation_id: &str, content: &str) -> Result<String, HostError> {
  let content = content.trim();
  if content.is_empty() || content.len() > 32 * 1024 {
    return Err(HostError::new("Enter a message no larger than 32 KB."));
  }
  state::idle()?;
  let request_id = apell_guest::create_id();
  let assistant_id = apell_guest::create_id();
  let (messages, target) = prepare(conversation_id, content, &request_id, &assistant_id).await?;
  state::start(state::Reply {
    request_id: request_id.clone(),
    conversation_id: conversation_id.to_owned(),
    assistant_id: assistant_id.clone(),
    partial: String::new(),
    stopping: false,
  });
  let job_id = request_id.clone();
  let message_id = assistant_id.clone();
  if let Err(error) = apell_guest::spawn(async move {
    let result = apell_guest::call_with_events(
      "llm.generate",
      json!({
        "request_id": job_id, "messages": messages, "target": target
      }),
      state::progress,
    )
    .await;
    storage::serialized(async {
      let result = persist_result(&job_id, &message_id, result).await;
      state::finish(&job_id, result.err());
    })
    .await;
  }) {
    state::finish(&request_id, None);
    storage::execute(
      "UPDATE messages SET status = 'interrupted' WHERE message_id = ?1 AND status = 'pending'",
      vec![storage::text(assistant_id)],
      None,
    )
    .await?;
    return Err(error);
  }
  Ok(request_id)
}

async fn persist_result(
  request_id: &str,
  assistant_id: &str,
  result: Result<Completion, HostError>,
) -> Result<(), HostError> {
  let provider_message = result.as_ref().ok().and_then(|completion| {
    if matches!(completion.status.as_str(), "completed" | "tool_calls") {
      completion
        .message
        .as_ref()
        .filter(|message| is_assistant_message(message))
    } else {
      None
    }
  });
  let completed = result
    .as_ref()
    .is_ok_and(|completion| completion.status == "completed" && provider_message.is_some());
  let content = match &result {
    Ok(completion) => completion.content.clone(),
    Err(error) if !error.partial_content.is_empty() => error.partial_content.clone(),
    Err(_) => state::partial(request_id),
  };
  storage::execute(
    "UPDATE messages SET content = ?1, status = ?2, provider_message = ?3, provenance = ?4 WHERE message_id = ?5 AND status = 'pending'",
    vec![
      storage::text(content),
      storage::text(if completed { "complete" } else { "interrupted" }),
      storage::optional_text(provider_message.map(Value::to_string)),
      storage::optional_text(result.as_ref().ok().map(|completion| completion.provenance.to_string())),
      storage::text(assistant_id),
    ],
    None,
  )
  .await?;
  match result {
    Ok(_) if completed => Ok(()),
    Ok(completion) => Err(HostError::new(
      completion
        .error
        .map(|error| error.message)
        .unwrap_or_else(|| {
          match completion.status.as_str() {
            "completed" => "The model did not return a replayable assistant message.",
            "cancelled" => "The reply was stopped.",
            "truncated" => "The model reached its response limit before finishing.",
            "tool_calls" => "This chatbot requires a text reply.",
            _ => "The model did not complete its reply.",
          }
          .to_owned()
        }),
    )),
    Err(error) => Err(error),
  }
}

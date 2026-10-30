use crate::{bridge, models::title_from_message, storage};
use serde::Deserialize;
use serde_json::{json, Value};
use std::cell::Cell;
use wasm_bindgen::JsValue;

thread_local! {
  static SENDING: Cell<bool> = const { Cell::new(false) };
}

struct SendingGuard;
impl Drop for SendingGuard {
  fn drop(&mut self) {
    SENDING.with(|sending| sending.set(false));
  }
}

#[derive(Deserialize)]
struct Completion {
  content: String,
}

async fn prepare(
  conversation_id: &str,
  content: &str,
  request_id: &str,
  assistant_id: &str,
) -> Result<Vec<Value>, JsValue> {
  let transaction = storage::begin().await?;
  let result = async {
    let conversation = storage::query("SELECT conversation_id FROM conversations WHERE conversation_id = ?1", vec![storage::text(conversation_id)], Some(&transaction)).await?;
    if conversation.is_empty() { return Err(bridge::error("This conversation no longer exists.")); }
    let rows = storage::query("SELECT role, content, status FROM messages WHERE conversation_id = ?1 ORDER BY rowid LIMIT 1001", vec![storage::text(conversation_id)], Some(&transaction)).await?;
    if rows.len() >= 1000 { return Err(bridge::error("This conversation is full. Start a new conversation.")); }
    let mut messages = vec![json!({"role": "system", "content": "You are a helpful, thoughtful assistant. Answer clearly and directly."})];
    for row in &rows {
      match storage::cell(row, 2)?.as_str() {
        "pending" => return Err(bridge::error("This conversation already has a reply in progress.")),
        "complete" => messages.push(json!({"role": storage::cell(row, 0)?, "content": storage::cell(row, 1)?})),
        _ => {}
      }
    }
    messages.push(json!({"role": "user", "content": content}));
    let now = bridge::now();
    storage::execute("INSERT INTO messages (message_id, conversation_id, role, content, status, request_id, created_at) VALUES (?1, ?2, 'user', ?3, 'complete', ?4, ?5)", vec![storage::text(bridge::create_id()), storage::text(conversation_id), storage::text(content), storage::text(request_id), storage::text(&now)], Some(&transaction)).await?;
    storage::execute("INSERT INTO messages (message_id, conversation_id, role, content, status, request_id, created_at) VALUES (?1, ?2, 'assistant', '', 'pending', ?3, ?4)", vec![storage::text(assistant_id), storage::text(conversation_id), storage::text(request_id), storage::text(&now)], Some(&transaction)).await?;
    let title = if rows.is_empty() { title_from_message(content) } else { String::new() };
    storage::execute("UPDATE conversations SET updated_at = ?1, title = CASE WHEN ?2 = '' THEN title ELSE ?2 END WHERE conversation_id = ?3", vec![storage::text(now), storage::text(title), storage::text(conversation_id)], Some(&transaction)).await?;
    storage::commit(&transaction).await?;
    Ok(messages)
  }.await;
  if result.is_err() {
    storage::rollback(&transaction).await;
  }
  result
}

pub async fn send(conversation_id: &str, content: &str, request_id: &str) -> Result<(), JsValue> {
  let content = content.trim();
  if content.is_empty() || content.len() > 32 * 1024 {
    return Err(bridge::error("Enter a message no larger than 32 KB."));
  }
  if SENDING.with(|sending| sending.replace(true)) {
    return Err(bridge::error(
      "Wait for the current reply or stop it first.",
    ));
  }
  let _guard = SendingGuard;
  let assistant_id = bridge::create_id();
  let messages = prepare(conversation_id, content, request_id, &assistant_id).await?;
  let result: Result<Completion, JsValue> = bridge::call(
    "llm.complete",
    json!({"request_id": request_id, "messages": messages}),
  )
  .await;
  let (content, status) = match &result {
    Ok(completion) => (completion.content.clone(), "complete"),
    Err(error) => (bridge::property(error, "partial_content"), "interrupted"),
  };
  storage::execute(
    "UPDATE messages SET content = ?1, status = ?2 WHERE message_id = ?3 AND status = 'pending'",
    vec![
      storage::text(content),
      storage::text(status),
      storage::text(assistant_id),
    ],
    None,
  )
  .await?;
  result.map(|_| ())
}

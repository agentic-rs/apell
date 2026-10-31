use crate::{
  bridge,
  models::{Conversation, Message},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::cell::RefCell;
use wasm_bindgen::JsValue;

thread_local! {
  static DATABASE: RefCell<Option<String>> = const { RefCell::new(None) };
}

#[derive(Deserialize)]
struct Opened {
  database_handle: String,
}

#[derive(Deserialize)]
struct Transaction {
  transaction_handle: String,
}

#[derive(Deserialize)]
struct Rows {
  rows: Vec<Vec<Value>>,
}

fn database() -> Result<String, JsValue> {
  DATABASE
    .with(|database| database.borrow().clone())
    .ok_or_else(|| bridge::error("The chatbot database is not open."))
}

pub fn text(value: impl Into<String>) -> Value {
  json!({"type": "text", "value": value.into()})
}

pub fn optional_text(value: Option<String>) -> Value {
  value.map(text).unwrap_or_else(|| json!({"type": "null"}))
}

pub fn cell(row: &[Value], index: usize) -> Result<String, JsValue> {
  row
    .get(index)
    .and_then(|value| value.get("value"))
    .and_then(Value::as_str)
    .map(str::to_owned)
    .ok_or_else(|| bridge::error("The chatbot database returned an invalid value."))
}

pub fn optional_cell(row: &[Value], index: usize) -> Result<Option<String>, JsValue> {
  if row
    .get(index)
    .and_then(|value| value.get("type"))
    .and_then(Value::as_str)
    == Some("null")
  {
    Ok(None)
  } else {
    cell(row, index).map(Some)
  }
}

pub async fn query(
  sql: &str,
  parameters: Vec<Value>,
  transaction: Option<&str>,
) -> Result<Vec<Vec<Value>>, JsValue> {
  let result: Rows = bridge::call(
    "sqlite.query",
    json!({
      "database_handle": database()?, "transaction_handle": transaction,
      "sql": sql, "parameters": parameters,
    }),
  )
  .await?;
  Ok(result.rows)
}

pub async fn execute(
  sql: &str,
  parameters: Vec<Value>,
  transaction: Option<&str>,
) -> Result<(), JsValue> {
  let _: Value = bridge::call(
    "sqlite.execute",
    json!({
      "database_handle": database()?, "transaction_handle": transaction,
      "sql": sql, "parameters": parameters,
    }),
  )
  .await?;
  Ok(())
}

pub async fn begin() -> Result<String, JsValue> {
  let result: Transaction =
    bridge::call("sqlite.begin", json!({"database_handle": database()?})).await?;
  Ok(result.transaction_handle)
}

pub async fn commit(transaction: &str) -> Result<(), JsValue> {
  let _: Value = bridge::call(
    "sqlite.commit",
    json!({
      "database_handle": database()?, "transaction_handle": transaction,
    }),
  )
  .await?;
  Ok(())
}

pub async fn rollback(transaction: &str) {
  if let Ok(database_handle) = database() {
    let _: Result<Value, JsValue> = bridge::call(
      "sqlite.rollback",
      json!({
        "database_handle": database_handle, "transaction_handle": transaction,
      }),
    )
    .await;
  }
}

pub async fn initialize() -> Result<(), JsValue> {
  let result: Opened = bridge::call("sqlite.open", json!({"database_name": "chatbot"})).await?;
  DATABASE.with(|database| database.replace(Some(result.database_handle)));
  let transaction = begin().await?;
  let result = async {
    let version = query("PRAGMA user_version", vec![], Some(&transaction)).await?;
    let version = cell(version.first().ok_or_else(|| bridge::error("Missing schema version."))?, 0)?;
    match version.as_str() {
      "0" => {
        execute("CREATE TABLE conversations (conversation_id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)", vec![], Some(&transaction)).await?;
        execute("CREATE TABLE messages (message_id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL REFERENCES conversations(conversation_id) ON DELETE CASCADE, role TEXT NOT NULL CHECK(role IN ('user', 'assistant')), content TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('pending', 'complete', 'interrupted')), request_id TEXT NOT NULL, created_at TEXT NOT NULL, provider_message TEXT, provenance TEXT)", vec![], Some(&transaction)).await?;
        execute("CREATE INDEX messages_by_conversation ON messages(conversation_id)", vec![], Some(&transaction)).await?;
        execute("PRAGMA user_version = 2", vec![], Some(&transaction)).await?;
      }
      "1" => {
        execute("ALTER TABLE messages ADD COLUMN provider_message TEXT", vec![], Some(&transaction)).await?;
        execute("ALTER TABLE messages ADD COLUMN provenance TEXT", vec![], Some(&transaction)).await?;
        execute("PRAGMA user_version = 2", vec![], Some(&transaction)).await?;
      }
      "2" => {}
      _ => return Err(bridge::error("This chatbot database requires a newer applet.")),
    }
    execute("UPDATE messages SET status = 'interrupted' WHERE status = 'pending'", vec![], Some(&transaction)).await?;
    commit(&transaction).await
  }.await;
  if result.is_err() {
    rollback(&transaction).await;
  }
  result
}

pub async fn conversations() -> Result<Vec<Conversation>, JsValue> {
  query("SELECT conversation_id, title, updated_at FROM conversations ORDER BY updated_at DESC, rowid DESC LIMIT 500", vec![], None).await?
    .into_iter().map(|row| Ok(Conversation {
      conversation_id: cell(&row, 0)?, title: cell(&row, 1)?, updated_at: cell(&row, 2)?,
    })).collect()
}

pub async fn create_conversation() -> Result<String, JsValue> {
  let conversation_id = bridge::create_id();
  let now = bridge::now();
  execute("INSERT INTO conversations (conversation_id, title, created_at, updated_at) VALUES (?1, 'New conversation', ?2, ?2)", vec![text(&conversation_id), text(now)], None).await?;
  Ok(conversation_id)
}

pub async fn messages(conversation_id: &str) -> Result<Vec<Message>, JsValue> {
  query("SELECT message_id, role, content, status FROM messages WHERE conversation_id = ?1 ORDER BY rowid LIMIT 1000", vec![text(conversation_id)], None).await?
    .into_iter().map(|row| Ok(Message {
      message_id: cell(&row, 0)?, role: cell(&row, 1)?, content: cell(&row, 2)?, status: cell(&row, 3)?,
    })).collect()
}

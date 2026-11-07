use crate::models::{Conversation, Media, Message};
use apell_guest::HostError;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{cell::RefCell, future::Future, rc::Rc};

thread_local! {
  static DATABASE: RefCell<Option<String>> = const { RefCell::new(None) };
  static DB_GATE: Rc<futures_util::lock::Mutex<()>> = Rc::default();
}

// A transaction on a handle must finish before another guest operation uses it.
// Generation itself runs outside this gate so attached views can still read.
pub async fn serialized<T>(operation: impl Future<Output = T>) -> T {
  let gate = DB_GATE.with(Rc::clone);
  let _guard = gate.lock().await;
  operation.await
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

fn database() -> Result<String, HostError> {
  DATABASE
    .with(|database| database.borrow().clone())
    .ok_or_else(|| HostError::new("The chatbot database is not open."))
}

pub fn text(value: impl Into<String>) -> Value {
  json!({"type": "text", "value": value.into()})
}

pub fn optional_text(value: Option<String>) -> Value {
  value.map(text).unwrap_or_else(|| json!({"type": "null"}))
}

pub fn cell(row: &[Value], index: usize) -> Result<String, HostError> {
  row
    .get(index)
    .and_then(|value| value.get("value"))
    .and_then(Value::as_str)
    .map(str::to_owned)
    .ok_or_else(|| HostError::new("The chatbot database returned an invalid value."))
}

pub fn optional_cell(row: &[Value], index: usize) -> Result<Option<String>, HostError> {
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
) -> Result<Vec<Vec<Value>>, HostError> {
  let result: Rows = apell_guest::call(
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
) -> Result<(), HostError> {
  let _: Value = apell_guest::call(
    "sqlite.execute",
    json!({
      "database_handle": database()?, "transaction_handle": transaction,
      "sql": sql, "parameters": parameters,
    }),
  )
  .await?;
  Ok(())
}

pub async fn begin() -> Result<String, HostError> {
  let result: Transaction =
    apell_guest::call("sqlite.begin", json!({"database_handle": database()?})).await?;
  Ok(result.transaction_handle)
}

pub async fn commit(transaction: &str) -> Result<(), HostError> {
  let _: Value = apell_guest::call(
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
    let _: Result<Value, HostError> = apell_guest::call(
      "sqlite.rollback",
      json!({
        "database_handle": database_handle, "transaction_handle": transaction,
      }),
    )
    .await;
  }
}

pub async fn initialize() -> Result<(), HostError> {
  if DATABASE.with(|database| database.borrow().is_some()) {
    return Ok(());
  }
  let result: Opened =
    apell_guest::call("sqlite.open", json!({"database_name": "chatbot"})).await?;
  DATABASE.with(|database| database.replace(Some(result.database_handle.clone())));
  let migration = migrate().await;
  if migration.is_err() {
    let _: Result<Value, HostError> = apell_guest::call(
      "sqlite.close",
      json!({"database_handle": result.database_handle}),
    )
    .await;
    DATABASE.with(|database| database.replace(None));
  }
  migration
}

async fn migrate() -> Result<(), HostError> {
  let transaction = begin().await?;
  let result = async {
    let version = query("PRAGMA user_version", vec![], Some(&transaction)).await?;
    let version = cell(version.first().ok_or_else(|| HostError::new("Missing schema version."))?, 0)?;
    match version.as_str() {
      "0" => {
        execute("CREATE TABLE conversations (conversation_id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)", vec![], Some(&transaction)).await?;
        execute("CREATE TABLE messages (message_id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL REFERENCES conversations(conversation_id) ON DELETE CASCADE, role TEXT NOT NULL CHECK(role IN ('user', 'assistant')), content TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('pending', 'complete', 'interrupted')), request_id TEXT NOT NULL, created_at TEXT NOT NULL, provider_message TEXT, provenance TEXT)", vec![], Some(&transaction)).await?;
        execute("CREATE INDEX messages_by_conversation ON messages(conversation_id)", vec![], Some(&transaction)).await?;
      }
      "1" => {
        execute("ALTER TABLE messages ADD COLUMN provider_message TEXT", vec![], Some(&transaction)).await?;
        execute("ALTER TABLE messages ADD COLUMN provenance TEXT", vec![], Some(&transaction)).await?;
      }
      "2" | "3" | "4" => {}
      _ => return Err(HostError::new("This chatbot database requires a newer applet.")),
    }
    if matches!(version.as_str(), "0" | "1" | "2") {
      execute("ALTER TABLE conversations ADD COLUMN model_target TEXT", vec![], Some(&transaction)).await?;
    }
    if version != "4" {
      execute("ALTER TABLE messages ADD COLUMN media TEXT NOT NULL DEFAULT '[]'", vec![], Some(&transaction)).await?;
      execute("PRAGMA user_version = 4", vec![], Some(&transaction)).await?;
    }
    execute("UPDATE messages SET status = 'interrupted' WHERE status = 'pending'", vec![], Some(&transaction)).await?;
    commit(&transaction).await
  }.await;
  if result.is_err() {
    rollback(&transaction).await;
  }
  result
}

pub async fn conversations() -> Result<Vec<Conversation>, HostError> {
  query("SELECT conversation_id, title, updated_at FROM conversations ORDER BY updated_at DESC, rowid DESC LIMIT 500", vec![], None).await?
    .into_iter().map(|row| Ok(Conversation {
      conversation_id: cell(&row, 0)?, title: cell(&row, 1)?, updated_at: cell(&row, 2)?,
    })).collect()
}

pub async fn messages(conversation_id: &str) -> Result<Vec<Message>, HostError> {
  query("SELECT message_id, role, content, status, media FROM messages WHERE conversation_id = ?1 ORDER BY rowid LIMIT 1000", vec![text(conversation_id)], None).await?
    .into_iter().map(|row| Ok(Message {
      message_id: cell(&row, 0)?, role: cell(&row, 1)?, content: cell(&row, 2)?, status: cell(&row, 3)?,
      media: media(&row, 4)?,
    })).collect()
}

pub fn media(row: &[Value], index: usize) -> Result<Vec<Media>, HostError> {
  serde_json::from_str(&cell(row, index)?)
    .map_err(|_| HostError::new("A stored media attachment cannot be read."))
}

mod bridge;
mod chat;
mod models;
mod selection;
mod storage;

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub async fn initialize() -> Result<(), JsValue> {
  storage::initialize().await
}

#[wasm_bindgen]
pub async fn list_conversations() -> Result<JsValue, JsValue> {
  bridge::output(&storage::conversations().await?)
}

#[wasm_bindgen]
pub async fn create_conversation() -> Result<String, JsValue> {
  storage::create_conversation(None).await
}

#[wasm_bindgen]
pub async fn conversation_model(conversation_id: String) -> Result<JsValue, JsValue> {
  bridge::output(&selection::load(&conversation_id, None).await?)
}

#[wasm_bindgen]
pub async fn choose_model(conversation_id: String, selection: JsValue) -> Result<String, JsValue> {
  let selection = serde_wasm_bindgen::from_value(selection).map_err(bridge::error)?;
  selection::choose(&conversation_id, selection).await
}

#[wasm_bindgen]
pub async fn load_messages(conversation_id: String) -> Result<JsValue, JsValue> {
  bridge::output(&storage::messages(&conversation_id).await?)
}

#[wasm_bindgen]
pub async fn send_message(
  conversation_id: String,
  content: String,
  request_id: String,
) -> Result<(), JsValue> {
  chat::send(&conversation_id, &content, &request_id).await
}

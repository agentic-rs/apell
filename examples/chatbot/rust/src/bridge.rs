use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen]
extern "C" {
  #[wasm_bindgen(js_namespace = globalThis, js_name = apellHostCall)]
  fn host_call(method: &str, params: JsValue) -> js_sys::Promise;

  #[wasm_bindgen(js_namespace = globalThis, js_name = apellCreateId)]
  pub fn create_id() -> String;
}

pub async fn call<T: DeserializeOwned>(method: &str, params: Value) -> Result<T, JsValue> {
  let params = params
    .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
    .map_err(error)?;
  let result = JsFuture::from(host_call(method, params)).await?;
  serde_wasm_bindgen::from_value(result).map_err(error)
}

pub fn error(value: impl std::fmt::Display) -> JsValue {
  js_sys::Error::new(&value.to_string()).into()
}

pub fn property(value: &JsValue, name: &str) -> String {
  js_sys::Reflect::get(value, &JsValue::from_str(name))
    .ok()
    .and_then(|value| value.as_string())
    .unwrap_or_default()
}

pub fn now() -> String {
  js_sys::Date::new_0()
    .to_iso_string()
    .as_string()
    .unwrap_or_default()
}

pub fn output(value: &impl serde::Serialize) -> Result<JsValue, JsValue> {
  value
    .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
    .map_err(error)
}

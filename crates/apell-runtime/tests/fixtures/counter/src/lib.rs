// This state is inside wasm linear memory, never in the interpreter's host data.
use apell_guest::{call, call_with_events, emit, spawn, HostError};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};

thread_local! {
  static COUNTER: Cell<u32> = const { Cell::new(0) };
  static LAST: RefCell<Value> = const { RefCell::new(Value::Null) };
}

async fn finish_delay() -> Result<Value, HostError> {
  let value: Value = call_with_events("test.delay", Value::Null, |event| {
    emit(json!({"name":"progress","payload":event})).unwrap();
  })
  .await?;
  LAST.with(|last| last.replace(value.clone()));
  emit(json!({"name":"finished","payload":value}))?;
  Ok(value)
}

async fn handle(command: String, params: Value) -> Result<Value, HostError> {
  match command.as_str() {
    "increment" => Ok(COUNTER.with(|counter| {
      counter.set(counter.get() + 1);
      json!(counter.get())
    })),
    "snapshot" => {
      Ok(json!({"counter":COUNTER.with(Cell::get), "last":LAST.with(|last|last.borrow().clone())}))
    }
    "start" => {
      spawn(async {
        let _ = finish_delay().await;
      })?;
      Ok(json!("started"))
    }
    "wait" => finish_delay().await,
    "echo" => call("test.echo", params).await,
    "metadata" => Ok(json!({"id":apell_guest::create_id(),"now":apell_guest::now()})),
    "stall" => loop {
      std::hint::spin_loop();
    },
    _ => Err(HostError::new("Unknown fixture command.")),
  }
}
apell_guest::export!(handle);

// This state is inside wasm linear memory, never in the interpreter's host data.
use apell_guest::{call, call_with_events, emit, services::Socket, spawn, HostError};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};

thread_local! {
  static COUNTER: Cell<u32> = const { Cell::new(0) };
  static LAST: RefCell<Value> = const { RefCell::new(Value::Null) };
  static RECORD: RefCell<Option<String>> = const { RefCell::new(None) };
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
    "receive_record" | "echo_record" => {
      let socket = Socket {
        socket_handle: "fixture\"socket".into(),
      };
      let data = socket
        .receive(1000)
        .await?
        .ok_or_else(|| HostError::new("Fixture closed."))?;
      let len = data.len();
      if command == "echo_record" {
        socket.send(&data).await?;
      } else {
        RECORD.with(|record| record.replace(Some(data)));
      }
      Ok(json!(len))
    }
    "send_record" => {
      let socket = Socket {
        socket_handle: "fixture\"socket".into(),
      };
      let data = match params {
        Value::String(data) => data,
        _ => RECORD
          .with(RefCell::take)
          .ok_or_else(|| HostError::new("No staged record."))?,
      };
      socket.send(&data).await?;
      Ok(Value::Null)
    }
    "metadata" => Ok(json!({"id":apell_guest::create_id(),"now":apell_guest::now()})),
    "stall" => loop {
      std::hint::spin_loop();
    },
    _ => Err(HostError::new("Unknown fixture command.")),
  }
}
apell_guest::export!(handle);

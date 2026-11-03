use apell_guest::HostError;
use serde::Serialize;
use serde_json::{json, Value};
use std::cell::RefCell;

#[derive(Serialize)]
pub struct Reply {
  pub request_id: String,
  pub conversation_id: String,
  pub assistant_id: String,
  pub partial: String,
  pub stopping: bool,
}

#[derive(Default, Serialize)]
struct State {
  revision: u64,
  conversation_id: Option<String>,
  active: Option<Reply>,
  error: Option<String>,
}

thread_local! {
  static STATE: RefCell<State> = RefCell::default();
}

pub fn snapshot() -> Value {
  STATE.with(|state| json!(&*state.borrow()))
}

pub fn conversation() -> Option<String> {
  STATE.with(|state| state.borrow().conversation_id.clone())
}

pub fn idle() -> Result<(), HostError> {
  if STATE.with(|state| state.borrow().active.is_some()) {
    Err(HostError::new(
      "Wait for the current reply or stop it first.",
    ))
  } else {
    Ok(())
  }
}

fn publish(state: &mut State, kind: &str, mut data: Value) {
  state.revision += 1;
  data["revision"] = json!(state.revision);
  data["kind"] = json!(kind);
  // Events are hints to attached views. The actor's snapshot is authoritative.
  let _ = apell_guest::emit(json!({"name":"chat_changed","payload":data}));
}

pub fn select(id: &str) {
  STATE.with_borrow_mut(|state| {
    state.conversation_id = Some(id.to_owned());
    state.error = None;
    publish(state, "view", json!({}));
  });
}

pub fn start(reply: Reply) {
  STATE.with_borrow_mut(|state| {
    state.conversation_id = Some(reply.conversation_id.clone());
    state.active = Some(reply);
    state.error = None;
    publish(state, "start", json!({}));
  });
}

pub fn progress(event: Value) {
  if event["kind"] != "text_delta" {
    return;
  }
  let (Some(id), Some(text)) = (event["request_id"].as_str(), event["text"].as_str()) else {
    return;
  };
  STATE.with_borrow_mut(|state| {
    if let Some(reply) = state.active.as_mut().filter(|reply| reply.request_id == id) {
      reply.partial.push_str(text);
      // A provider can return its entire answer in one chunk. Keep view
      // notifications small; the full partial is already in the actor snapshot.
      if text.len() > 16 * 1024 {
        publish(state, "view", json!({}));
      } else {
        publish(state, "delta", json!({"request_id":id,"text":text}));
      }
    }
  });
}

pub fn partial(id: &str) -> String {
  STATE.with_borrow(|state| {
    state
      .active
      .as_ref()
      .filter(|reply| reply.request_id == id)
      .map(|reply| reply.partial.clone())
      .unwrap_or_default()
  })
}

pub fn finish(id: &str, error: Option<HostError>) {
  STATE.with_borrow_mut(|state| {
    if state
      .active
      .as_ref()
      .is_some_and(|reply| reply.request_id == id)
    {
      state.active = None;
      state.error = error.map(|error| error.message);
      publish(state, "finish", json!({"request_id":id}));
    }
  });
}

pub async fn stop(id: &str) -> Result<(), HostError> {
  let should_cancel = STATE.with_borrow_mut(|state| {
    let Some(reply) = state
      .active
      .as_mut()
      .filter(|reply| reply.request_id == id && !reply.stopping)
    else {
      return false;
    };
    reply.stopping = true;
    publish(state, "stopping", json!({"request_id":id}));
    true
  });
  if should_cancel {
    let result: Result<Value, HostError> =
      apell_guest::call("llm.cancel", json!({"request_id":id})).await;
    if let Err(error) = result {
      STATE.with_borrow_mut(|state| {
        if let Some(reply) = state.active.as_mut().filter(|reply| reply.request_id == id) {
          reply.stopping = false;
          publish(state, "view", json!({}));
        }
      });
      return Err(error);
    }
  }
  Ok(())
}

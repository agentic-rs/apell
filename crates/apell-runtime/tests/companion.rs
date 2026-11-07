//! Execute Companion's distributed Rust guest against the real SQLite host service.
use apell_core::sqlite::SqliteService;
use apell_runtime::{HostError, HostEvents, HostServices, RuntimeHandle, ServiceFuture};
use serde_json::{json, Value};
use std::{
  path::{Path, PathBuf},
  process::Command,
  sync::{Arc, OnceLock},
  time::Duration,
};
use tokio::sync::{broadcast, mpsc, oneshot};

// Debug-mode interpretation of a large snapshot can run alongside other workspace tests.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

fn module() -> Vec<u8> {
  static MODULE: OnceLock<Vec<u8>> = OnceLock::new();
  MODULE
    .get_or_init(|| {
      let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/chatbot");
      let target = example.join("target/actor-tests");
      let status = Command::new(env!("CARGO"))
        .current_dir(&example)
        .args([
          "build",
          "--locked",
          "--offline",
          "--release",
          "--target",
          "wasm32-unknown-unknown",
          "--target-dir",
        ])
        .arg(&target)
        .status()
        .unwrap();
      assert!(status.success(), "could not build the Companion guest");
      std::fs::read(target.join("wasm32-unknown-unknown/release/chatbot.wasm")).unwrap()
    })
    .clone()
}

struct Generation {
  params: Value,
  events: HostEvents,
  complete: oneshot::Sender<Result<Value, HostError>>,
}

struct Services {
  sql: Arc<SqliteService>,
  start: mpsc::UnboundedSender<Generation>,
  cancel: mpsc::UnboundedSender<String>,
}

impl HostServices for Services {
  fn call(&self, method: String, params: Value, events: HostEvents) -> ServiceFuture {
    let sql = self.sql.clone();
    let start = self.start.clone();
    let cancel = self.cancel.clone();
    Box::pin(async move {
      match method.as_str() {
        method if method.starts_with("sqlite.") => sql
          .dispatch("actor", "chatbot", method, params)
          .map_err(HostError::new),
        "llm.config" => {
          Ok(json!({"mode":"demo","connection_id":"local-demo","model_id":"local-demo"}))
        }
        "llm.generate" => {
          let (complete, result) = oneshot::channel();
          start
            .send(Generation {
              params,
              events,
              complete,
            })
            .map_err(|_| HostError::new("fixture closed"))?;
          result.await.map_err(HostError::new)?
        }
        "llm.cancel" => {
          cancel
            .send(params["request_id"].as_str().unwrap().to_owned())
            .unwrap();
          Ok(Value::Null)
        }
        _ => Err(HostError::new("Unexpected host method.")),
      }
    })
  }
}

struct App {
  runtime: RuntimeHandle,
  requests: mpsc::UnboundedReceiver<Generation>,
  cancellations: mpsc::UnboundedReceiver<String>,
}

impl App {
  async fn launch(root: &Path) -> Self {
    let (start, requests) = mpsc::unbounded_channel();
    let (cancel, cancellations) = mpsc::unbounded_channel();
    let sql = Arc::new(SqliteService::new(root.to_owned()));
    sql.register_session("actor", "chatbot").unwrap();
    let runtime = RuntimeHandle::start(module(), Arc::new(Services { sql, start, cancel }))
      .await
      .unwrap();
    let app = Self {
      runtime,
      requests,
      cancellations,
    };
    app.invoke("initialize", json!({})).await;
    app
  }

  async fn invoke(&self, method: &str, params: Value) -> Value {
    tokio::time::timeout(TEST_TIMEOUT, self.runtime.invoke(method, params))
      .await
      .unwrap_or_else(|_| panic!("timed out waiting for {method}"))
      .unwrap_or_else(|error| panic!("{method}: {error}"))
  }

  async fn snapshot(&self) -> Value {
    self.invoke("snapshot", json!({})).await
  }

  async fn request(&mut self, id: &str, text: &str) -> Generation {
    self
      .request_with(json!({"conversation_id":id,"content":text}))
      .await
  }

  async fn request_with(&mut self, params: Value) -> Generation {
    let ack = self.invoke("start_send", params).await;
    let call = tokio::time::timeout(TEST_TIMEOUT, self.requests.recv())
      .await
      .unwrap()
      .unwrap();
    assert_eq!(ack["request_id"], call.params["request_id"]);
    call
  }
}

async fn event(rx: &mut broadcast::Receiver<Value>, kind: &str) -> Value {
  tokio::time::timeout(TEST_TIMEOUT, async {
    loop {
      let value = rx.recv().await.unwrap();
      if value["name"] == "chat_changed" && value["payload"]["kind"] == kind {
        return value["payload"].clone();
      }
    }
  })
  .await
  .unwrap()
}

fn target() -> Value {
  json!({"connection_id":"selected","model_id":"requested-model"})
}
fn result(call: &Generation, status: &str, text: &str, message: Value) -> Value {
  json!({"request_id":call.params["request_id"],"status":status,"content":text,"message":message,
    "provenance":{"connection_id":call.params["target"]["connection_id"],
      "requested_model_id":call.params["target"]["model_id"], "model_id":"actual-model"},
    "error": if status=="completed" { Value::Null } else {json!({"message":"The reply was stopped."})}})
}
fn delta(call: &Generation, text: &str) {
  call
    .events
    .emit(json!({"request_id":call.params["request_id"],"kind":"text_delta","text":text}))
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generation_survives_a_detached_view_and_replays_only_on_its_original_route() {
  let dir = tempfile::tempdir().unwrap();
  let mut app = App::launch(dir.path()).await;
  let original = app.snapshot().await;
  let id = original["conversation_id"].as_str().unwrap().to_owned();
  assert!(original["target"].is_null());
  assert_eq!(
    app
      .invoke(
        "choose_model",
        json!({"conversation_id":id,"target":target()})
      )
      .await,
    id
  );
  let mut first_view = app.runtime.subscribe();
  let first = app.request(&id, "First question").await;
  assert_eq!(first.params["target"], target());
  delta(&first, "Hello ");
  event(&mut first_view, "delta").await;
  drop(first_view);

  // This delta arrives with no attached event receiver.
  delta(&first, "again");
  // UI initialization may be retried; it must not mark a live reply interrupted.
  app.invoke("initialize", json!({})).await;
  let mut second_view = app.runtime.subscribe();
  let attached = app.snapshot().await;
  assert_eq!(attached["active"]["partial"], "Hello again");
  assert_eq!(attached["active"]["request_id"], first.params["request_id"]);
  assert_eq!(attached["messages"][1]["status"], "pending");
  let assistant = json!({"role":"assistant","content":"Hello again","reasoning_details":[{"signature":"fixture-secret"}]});
  let finished = result(&first, "completed", "Hello again", assistant.clone());
  first.complete.send(Ok(finished)).unwrap();
  event(&mut second_view, "finish").await;
  let done = app.snapshot().await;
  assert!(done["active"].is_null());
  assert_eq!(done["messages"].as_array().unwrap().len(), 2);
  assert_eq!(done["messages"][1]["status"], "complete");
  assert!(!done.to_string().contains("fixture-secret"));

  let followup = app.request(&id, "Follow up").await;
  assert_eq!(followup.params["messages"][2], assistant);
  let request_id = followup.params["request_id"].clone();
  app.invoke("stop", json!({"request_id":request_id})).await;
  assert_eq!(
    app.cancellations.recv().await.unwrap(),
    request_id.as_str().unwrap()
  );
  let interrupted = result(&followup, "cancelled", "Authoritative partial", Value::Null);
  followup.complete.send(Ok(interrupted)).unwrap();
  event(&mut second_view, "finish").await;
  let done = app.snapshot().await;
  assert_eq!(done["messages"][3]["content"], "Authoritative partial");
  assert_eq!(done["messages"][3]["status"], "interrupted");

  let next=app.invoke("choose_model",json!({"conversation_id":id,"target":{"connection_id":"another","model_id":"requested-model"}})).await;
  assert_ne!(next, id);
  assert_eq!(app.snapshot().await["messages"], json!([]));
  app.runtime.stop().await;
  let reopened = App::launch(dir.path()).await;
  reopened
    .invoke("open_conversation", json!({"conversation_id":id}))
    .await;
  assert_eq!(reopened.snapshot().await["messages"], done["messages"]);
  reopened.runtime.stop().await;
}

fn seed_history(root: &Path, version: u32, attributions: &[Option<Value>]) {
  let sql = SqliteService::new(root.to_owned());
  sql.register_session("seed", "chatbot").unwrap();
  let db = sql
    .dispatch(
      "seed",
      "chatbot",
      "sqlite.open",
      json!({"database_name":"chatbot"}),
    )
    .unwrap();
  let execute = |statement: &str, parameters: Value| {
    sql
      .dispatch(
        "seed",
        "chatbot",
        "sqlite.execute",
        json!({"database_handle":db["database_handle"],"sql":statement,"parameters":parameters}),
      )
      .unwrap();
  };
  let text = |s: &str| json!({"type":"text","value":s});
  execute("CREATE TABLE conversations (conversation_id TEXT PRIMARY KEY,title TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL)",json!([]));
  execute("CREATE TABLE messages (message_id TEXT PRIMARY KEY,conversation_id TEXT NOT NULL,role TEXT NOT NULL,content TEXT NOT NULL,status TEXT NOT NULL,request_id TEXT NOT NULL,created_at TEXT NOT NULL)",json!([]));
  if version >= 2 {
    execute(
      "ALTER TABLE messages ADD COLUMN provider_message TEXT",
      json!([]),
    );
    execute("ALTER TABLE messages ADD COLUMN provenance TEXT", json!([]));
  }
  if version >= 3 {
    execute(
      "ALTER TABLE conversations ADD COLUMN model_target TEXT",
      json!([]),
    );
  }
  execute(
    "INSERT INTO conversations (conversation_id,title,created_at,updated_at) VALUES ('old','Old chat','then','then')",
    json!([]),
  );
  for (index, attribution) in attributions.iter().enumerate() {
    let id = format!("old_{index}");
    execute("INSERT INTO messages (message_id,conversation_id,role,content,status,request_id,created_at) VALUES (?1,'old','assistant','Prior answer','complete','previous','then')",json!([text(&id)]));
    if let Some(a) = attribution {
      execute(
        "UPDATE messages SET provenance=?1 WHERE message_id=?2",
        json!([text(&a.to_string()), text(&id)]),
      );
    }
  }
  execute(&format!("PRAGMA user_version = {version}"), json!([]));
  sql.close_session("seed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_migration_keeps_old_chats_and_only_replays_proven_history() {
  let attribution = json!({"connection_id":"selected","requested_model_id":"requested-model","model_id":"reported-model"});
  let other = json!({"connection_id":"unrelated","requested_model_id":"requested-model"});
  let demo = json!({"connection_id":"local-demo","requested_model_id":"local-demo"});
  for (version, rows, infer) in [
    (1, vec![None], false),
    (
      2,
      vec![Some(attribution.clone()), Some(attribution.clone())],
      true,
    ),
    (3, vec![Some(attribution.clone())], true),
    (2, vec![Some(attribution.clone()), Some(other)], false),
    (2, vec![Some(attribution), None], false),
    (2, vec![Some(demo)], false),
  ] {
    let dir = tempfile::tempdir().unwrap();
    seed_history(dir.path(), version, &rows);
    let app = App::launch(dir.path()).await;
    let view = app.snapshot().await;
    assert_eq!(view["messages"].as_array().unwrap().len(), rows.len());
    assert!(view["messages"]
      .as_array()
      .unwrap()
      .iter()
      .all(|message| message["media"] == json!([])));
    assert_eq!(view["target"], if infer { target() } else { Value::Null });
    let id = app
      .invoke(
        "choose_model",
        json!({"conversation_id":"old","target":target()}),
      )
      .await;
    assert_eq!(id == "old", infer);
    if !infer {
      assert_eq!(app.snapshot().await["messages"], json!([]));
      app
        .invoke("open_conversation", json!({"conversation_id":"old"}))
        .await;
      assert_eq!(app.snapshot().await["messages"], view["messages"]);
    }
    app.runtime.stop().await;
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cold_actor_recovers_an_abandoned_reply_after_shutdown() {
  let dir = tempfile::tempdir().unwrap();
  let mut app = App::launch(dir.path()).await;
  let id = app.snapshot().await["conversation_id"]
    .as_str()
    .unwrap()
    .to_owned();
  app
    .invoke(
      "choose_model",
      json!({"conversation_id":id,"target":target()}),
    )
    .await;
  let _unfinished = app.request(&id, "In progress").await;
  assert_eq!(app.snapshot().await["messages"][1]["status"], "pending");
  app.runtime.stop().await;
  let reopened = App::launch(dir.path()).await;
  let recovered = reopened.snapshot().await;
  assert!(recovered["active"].is_null());
  assert_eq!(recovered["messages"][1]["status"], "interrupted");
  reopened.runtime.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_replies_stay_visible_without_replaying_provider_metadata() {
  let dir = tempfile::tempdir().unwrap();
  let mut app = App::launch(dir.path()).await;
  let id = app.snapshot().await["conversation_id"]
    .as_str()
    .unwrap()
    .to_owned();
  app
    .invoke(
      "choose_model",
      json!({"conversation_id":id,"target":target()}),
    )
    .await;
  let mut view = app.runtime.subscribe();
  for status in [
    "tool_calls",
    "failed",
    "truncated",
    "unsupported",
    "missing_message",
    "transport",
  ] {
    let call = app.request(&id, "Question").await;
    assert!(call.params["messages"]
      .as_array()
      .unwrap()
      .iter()
      .all(|m| m["role"] != "assistant"));
    if status == "transport" {
      delta(&call, "Received before transport failure");
      call
        .complete
        .send(Err(HostError::new("Connection closed.")))
        .unwrap();
    } else {
      let message = if status == "tool_calls" {
        json!({"role":"assistant","tool_calls":[{"id":"call_one","type":"function","function":{"name":"lookup","arguments":"{}"}}],"reasoning_content":"private-tool-reasoning"})
      } else {
        Value::Null
      };
      let result = result(
        &call,
        if status == "missing_message" {
          "completed"
        } else {
          status
        },
        "Visible partial",
        message,
      );
      call.complete.send(Ok(result)).unwrap();
    }
    event(&mut view, "finish").await;
    let snapshot = app.snapshot().await;
    let last = snapshot["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last["status"], "interrupted");
    assert_eq!(
      last["content"],
      if status == "transport" {
        "Received before transport failure"
      } else {
        "Visible partial"
      }
    );
    assert!(!snapshot.to_string().contains("private-tool-reasoning"));
  }
  app.runtime.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_provider_chunk_is_recovered_from_snapshot_without_oversizing_ui_events() {
  let dir = tempfile::tempdir().unwrap();
  let mut app = App::launch(dir.path()).await;
  let id = app.snapshot().await["conversation_id"]
    .as_str()
    .unwrap()
    .to_owned();
  app
    .invoke(
      "choose_model",
      json!({"conversation_id":id,"target":target()}),
    )
    .await;
  let mut view = app.runtime.subscribe();
  let call = app.request(&id, "Write a long reply").await;
  let chunk = "x".repeat(260 * 1024);
  delta(&call, &chunk);
  let update = event(&mut view, "view").await;
  assert!(update.get("text").is_none());
  assert_eq!(app.snapshot().await["active"]["partial"], chunk);
  app.runtime.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_image_and_audio_attachments_are_restored_as_structured_history() {
  let dir = tempfile::tempdir().unwrap();
  let mut app = App::launch(dir.path()).await;
  let id = app.snapshot().await["conversation_id"]
    .as_str()
    .unwrap()
    .to_owned();
  app
    .invoke(
      "choose_model",
      json!({"conversation_id":id,"target":target()}),
    )
    .await;
  let media = json!([
    {"type":"image","path":"attachments/photo.png","mime_type":"image/png","size_bytes":64,"file_name":"Photo.png"},
    {"type":"audio","path":"attachments/question.wav","mime_type":"audio/wav","size_bytes":128,"file_name":"Question.wav"}
  ]);
  let parts = json!([
    {"type":"image","path":"attachments/photo.png"},
    {"type":"audio","path":"attachments/question.wav"}
  ]);
  let mut view = app.runtime.subscribe();
  let call = app
    .request_with(json!({"conversation_id":id,"content":"","media":media}))
    .await;
  assert_eq!(
    call.params["messages"][1],
    json!({"role":"user","content":parts})
  );
  let assistant = json!({"role":"assistant","content":"Understood.","reasoning_details":[{"signature":"retained-provider-state"}]});
  let finished = result(&call, "completed", "Understood.", assistant.clone());
  call.complete.send(Ok(finished)).unwrap();
  event(&mut view, "finish").await;
  let stored = app.snapshot().await;
  assert_eq!(stored["messages"][0]["content"], "");
  assert_eq!(stored["messages"][0]["media"], media);
  assert_eq!(stored["conversations"][0]["title"], "Photo.png");
  app.runtime.stop().await;

  let mut reopened = App::launch(dir.path()).await;
  assert_eq!(reopened.snapshot().await["messages"], stored["messages"]);
  let followup = reopened.request(&id, "Follow up").await;
  assert_eq!(
    followup.params["messages"][1],
    json!({"role":"user","content":parts})
  );
  assert_eq!(followup.params["messages"][2], assistant);
  assert_eq!(followup.params["messages"][3]["content"], "Follow up");
  reopened.runtime.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn media_only_generated_images_and_audio_survive_reopening_and_replay_safely() {
  for (output, media, assistant) in [
    (
      json!({"type":"image","format":"webp"}),
      json!({"type":"image","path":"llm/image_fixture/1.webp","mime_type":"image/webp","size_bytes":256}),
      json!({"role":"assistant","content":null,"provider_state":{"version":1,"output":[]}}),
    ),
    (
      json!({"type":"audio","voice":"fixture_voice","format":"wav"}),
      json!({"type":"audio","path":"llm/audio_fixture/1.wav","mime_type":"audio/wav","size_bytes":512}),
      json!({"role":"assistant","content":null}),
    ),
  ] {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::launch(dir.path()).await;
    let id = app.snapshot().await["conversation_id"]
      .as_str()
      .unwrap()
      .to_owned();
    app
      .invoke(
        "choose_model",
        json!({"conversation_id":id,"target":target()}),
      )
      .await;
    let mut view = app.runtime.subscribe();
    let call = app
      .request_with(json!({"conversation_id":id,"content":"Generate a reply.","output":output}))
      .await;
    assert_eq!(call.params["output"], output);
    let mut finished = result(&call, "completed", "", assistant.clone());
    finished["media"] = json!([media]);
    call.complete.send(Ok(finished)).unwrap();
    event(&mut view, "finish").await;
    let stored = app.snapshot().await;
    assert_eq!(stored["messages"][1]["status"], "complete");
    assert_eq!(stored["messages"][1]["content"], "");
    assert_eq!(stored["messages"][1]["media"], json!([media]));
    app.runtime.stop().await;

    let mut reopened = App::launch(dir.path()).await;
    assert_eq!(reopened.snapshot().await["messages"], stored["messages"]);
    let followup = reopened.request(&id, "Continue").await;
    assert!(followup.params.get("output").is_none());
    let history = followup.params["messages"].as_array().unwrap();
    if output["type"] == "image" {
      assert_eq!(history[2], assistant);
    } else {
      assert!(history.iter().all(|message| message["role"] != "assistant"));
    }
    reopened.runtime.stop().await;
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachment_limits_reject_new_turns_before_creating_pending_rows() {
  let dir = tempfile::tempdir().unwrap();
  let mut app = App::launch(dir.path()).await;
  let id = app.snapshot().await["conversation_id"]
    .as_str()
    .unwrap()
    .to_owned();
  app
    .invoke(
      "choose_model",
      json!({"conversation_id":id,"target":target()}),
    )
    .await;
  let media: Vec<_> = (0..8).map(|index| json!({
    "type":"image", "path":format!("attachments/{index}.png"), "mime_type":"image/png", "size_bytes":1024*1024
  })).collect();
  let mut view = app.runtime.subscribe();
  let call = app
    .request_with(json!({"conversation_id":id,"content":"Inspect these images.","media":media}))
    .await;
  let finished = result(
    &call,
    "completed",
    "Inspected.",
    json!({"role":"assistant","content":"Inspected."}),
  );
  call.complete.send(Ok(finished)).unwrap();
  event(&mut view, "finish").await;
  let before = app.snapshot().await;
  for (attachment, expected_error) in [
    (
      json!({"type":"image","path":"attachments/ninth.png","mime_type":"image/png","size_bytes":1}),
      "context limit",
    ),
    (
      json!({"type":"audio","path":"attachments/oversized.wav","mime_type":"audio/wav","size_bytes":8*1024*1024+1}),
      "no larger than 8 MiB",
    ),
    (
      json!({"type":"image","path":"../other/photo.png","mime_type":"image/png","size_bytes":1}),
      "private",
    ),
  ] {
    let error = tokio::time::timeout(
      TEST_TIMEOUT,
      app.runtime.invoke(
        "start_send",
        json!({
          "conversation_id":id,"content":"Another file.","media":[attachment]
        }),
      ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.message.contains(expected_error), "{}", error.message);
    let after = app.snapshot().await;
    assert!(after["active"].is_null());
    assert_eq!(after["messages"], before["messages"]);
    assert!(app.requests.try_recv().is_err());
  }
  app.runtime.stop().await;
}

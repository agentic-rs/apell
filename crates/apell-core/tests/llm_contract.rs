use apell_core::llm::{
  LlmConfig, LlmEvent, LlmMessage, LlmRequest, LlmResult, LlmService, LlmStatus, LlmToolChoice,
};
use serde_json::{json, Value};
use std::{
  collections::VecDeque,
  sync::{Arc, Mutex},
  time::Duration,
};
use tokio::{
  io::{AsyncReadExt, AsyncWriteExt},
  net::{TcpListener, TcpStream},
};

const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct CapturedRequest {
  headers: String,
  body: Value,
}

enum Reply {
  Json(Value),
  HttpError(u16, Value),
  Stream(String),
  HoldStream(String),
}

struct Server {
  config: LlmConfig,
  captured: Arc<Mutex<Vec<CapturedRequest>>>,
  worker: tokio::task::JoinHandle<()>,
}

impl Server {
  async fn start(replies: Vec<Reply>) -> Self {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = LlmConfig {
      base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
      model_id: "requested-alias".into(),
      ..LlmConfig::default()
    };
    let captured = Arc::new(Mutex::new(Vec::new()));
    let received = captured.clone();
    let worker = tokio::spawn(async move {
      let mut replies = VecDeque::from(replies);
      loop {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request = tokio::time::timeout(DEADLINE, read_request(&mut stream))
          .await
          .expect("The client did not finish sending its request.");
        received.lock().unwrap().push(request);
        let reply = replies.pop_front().unwrap_or_else(|| {
          Reply::Json(json!({
            "error": { "message": "Unexpected request in test." }
          }))
        });
        let (status, content_type, body, hold) = match reply {
          Reply::Json(body) => (200, "application/json", body.to_string(), false),
          Reply::HttpError(status, body) => (status, "application/json", body.to_string(), false),
          Reply::Stream(body) => (200, "text/event-stream", body, false),
          Reply::HoldStream(body) => (200, "text/event-stream", body, true),
        };
        let length = if hold {
          String::new()
        } else {
          format!("Content-Length: {}\r\n", body.len())
        };
        let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\n{length}Connection: close\r\n\r\n{body}");
        stream.write_all(response.as_bytes()).await.unwrap();
        if hold {
          // Keep the response incomplete until cancellation closes the connection.
          // This synchronizes on the real transport instead of timing a sleep.
          let mut closed = Vec::new();
          let _ = tokio::time::timeout(DEADLINE, stream.read_to_end(&mut closed)).await;
        }
      }
    });
    Self {
      config,
      captured,
      worker,
    }
  }

  fn requests(&self) -> Vec<CapturedRequest> {
    self.captured.lock().unwrap().clone()
  }
}

impl Drop for Server {
  fn drop(&mut self) {
    self.worker.abort();
  }
}

async fn read_request(stream: &mut TcpStream) -> CapturedRequest {
  let mut bytes = Vec::new();
  let mut buffer = [0; 4096];
  let header_end = loop {
    if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
      break index + 4;
    }
    let received = stream.read(&mut buffer).await.unwrap();
    assert_ne!(received, 0, "Connection ended before request headers.");
    bytes.extend_from_slice(&buffer[..received]);
    assert!(bytes.len() <= 1024 * 1024);
  };
  let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
  let length = headers
    .lines()
    .find_map(|line| {
      let (name, value) = line.split_once(':')?;
      name
        .eq_ignore_ascii_case("content-length")
        .then(|| value.trim().parse::<usize>().unwrap())
    })
    .expect("JSON requests must have a content length.");
  assert!(length <= 1024 * 1024);
  while bytes.len() < header_end + length {
    let received = stream.read(&mut buffer).await.unwrap();
    assert_ne!(received, 0, "Connection ended before request body.");
    bytes.extend_from_slice(&buffer[..received]);
  }
  CapturedRequest {
    headers,
    body: serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap(),
  }
}

fn input(request_id: &str) -> LlmRequest {
  LlmRequest {
    request_id: request_id.into(),
    messages: vec![LlmMessage { role: "user".into(), content: "Check the fixture.".into(), ..LlmMessage::default() }],
    tools: vec![serde_json::from_value(json!({
      "name": "inspect_fixture",
      "description": "Inspect a local fixture.",
      "parameters": { "type": "object", "properties": { "label": { "type": "string" } }, "required": ["label"] }
    })).unwrap()],
    ..LlmRequest::default()
  }
}

fn assistant(result: &LlmResult) -> LlmMessage {
  LlmMessage {
    role: "assistant".into(),
    content: result.content.clone(),
    tool_calls: result.tool_calls.clone(),
    continuation_id: result.continuation_id.clone(),
    ..LlmMessage::default()
  }
}

fn followup(result: &LlmResult) -> LlmRequest {
  let mut request = input("followup");
  request.messages.push(assistant(result));
  request.messages.push(LlmMessage {
    role: "tool".into(),
    content: "{\"found\":true}".into(),
    tool_call_id: Some("call_fixture".into()),
    ..LlmMessage::default()
  });
  request
}

fn completion(message: Value, finish_reason: &str) -> Value {
  json!({
    "id": "completion_fixture",
    "model": "actual-model-revision",
    "choices": [{ "index": 0, "message": message, "finish_reason": finish_reason }]
  })
}

fn tool_response() -> Value {
  completion(
    json!({
      "role": "assistant",
      "content": null,
      "reasoning_content": "Visible provider reasoning.",
      "reasoning_details": [{ "type": "reasoning.encrypted", "data": "opaque-provider-state", "id": "reasoning_fixture", "index": 0 }],
      "tool_calls": [{ "id": "call_fixture", "type": "function", "function": { "name": "inspect_fixture", "arguments": "{\"label\":\"例\"}" } }]
    }),
    "tool_calls",
  )
}

fn sse(value: Value) -> String {
  format!("data: {value}\n\n")
}

async fn generate(
  service: &LlmService,
  session_id: &str,
  request: LlmRequest,
  config: &LlmConfig,
) -> (LlmResult, Vec<LlmEvent>) {
  let events = Arc::new(Mutex::new(Vec::new()));
  let emitted = events.clone();
  let result = tokio::time::timeout(
    DEADLINE,
    service.generate(
      session_id,
      request,
      config.clone(),
      Some("fixture-api-key".into()),
      move |event| emitted.lock().unwrap().push(event),
    ),
  )
  .await
  .expect("LLM generation did not finish.");
  let events = events.lock().unwrap().clone();
  let terminal: Vec<_> = events
    .iter()
    .filter(|event| event.kind == "terminal")
    .collect();
  assert_eq!(
    terminal.len(),
    1,
    "Every request must emit one terminal result."
  );
  assert_eq!(
    serde_json::to_value(terminal[0].result.as_ref().unwrap()).unwrap(),
    serde_json::to_value(&result).unwrap()
  );
  (result, events)
}

#[tokio::test]
async fn tool_history_and_all_assistant_reasoning_round_trip_without_exposing_provider_state() {
  let server = Server::start(vec![
    Reply::Json(tool_response()),
    Reply::Json(completion(json!({ "role": "assistant", "content": "The fixture exists.", "reasoning_content": "Second turn reasoning." }), "stop")),
    Reply::Json(completion(json!({ "role": "assistant", "content": "Still available." }), "stop")),
  ]).await;
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let mut request = input("tools");
  request.tool_choice = Some(LlmToolChoice::Named {
    name: "inspect_fixture".into(),
  });
  let (first, events) = generate(&service, "session", request, &server.config).await;
  assert_eq!(first.status, LlmStatus::ToolCalls);
  assert!(first.content.is_empty());
  assert_eq!(first.tool_calls[0].arguments, "{\"label\":\"例\"}");
  assert!(first.continuation_id.is_some());
  assert!(events
    .iter()
    .any(|event| event.kind == "reasoning_delta" && event.text == "Visible provider reasoning."));
  assert!(
    events.iter().any(
      |event| event.kind == "tool_call" && event.tool_call.as_ref() == first.tool_calls.first()
    )
  );
  assert!(!serde_json::to_string(&first)
    .unwrap()
    .contains("opaque-provider-state"));
  assert_eq!(first.provenance.connection_id, server.config.connection_id);
  assert_eq!(first.provenance.requested_model_id, "requested-alias");
  assert_eq!(
    first.provenance.model_id.as_deref(),
    Some("actual-model-revision")
  );

  let mut next = followup(&first);
  next.tool_choice = Some(LlmToolChoice::Mode("auto".into()));
  let (second, _) = generate(&service, "session", next.clone(), &server.config).await;
  assert_eq!(second.status, LlmStatus::Completed);
  assert!(second.continuation_id.is_some());
  next.request_id = "third_turn".into();
  next.messages.push(assistant(&second));
  next.messages.push(LlmMessage {
    role: "user".into(),
    content: "Check again.".into(),
    ..LlmMessage::default()
  });
  let (third, _) = generate(&service, "session", next, &server.config).await;
  assert_eq!(third.status, LlmStatus::Completed);

  let requests = server.requests();
  assert_eq!(requests.len(), 3);
  assert!(requests[0]
    .headers
    .starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
  assert!(requests[0]
    .headers
    .to_ascii_lowercase()
    .contains("authorization: bearer fixture-api-key"));
  assert_eq!(requests[0].body["tools"][0]["type"], "function");
  assert_eq!(
    requests[0].body["tools"][0]["function"]["name"],
    "inspect_fixture"
  );
  assert_eq!(
    requests[0].body["tool_choice"],
    json!({ "type": "function", "function": { "name": "inspect_fixture" } })
  );
  assert_eq!(requests[1].body["tool_choice"], "auto");
  for sent in &requests[1..] {
    assert_eq!(
      sent.body["messages"][1]["reasoning_content"],
      "Visible provider reasoning."
    );
    assert_eq!(
      sent.body["messages"][1]["reasoning_details"],
      tool_response()["choices"][0]["message"]["reasoning_details"]
    );
    assert_eq!(
      sent.body["messages"][1]["tool_calls"],
      tool_response()["choices"][0]["message"]["tool_calls"]
    );
    assert_eq!(
      sent.body["messages"][2],
      json!({ "role": "tool", "tool_call_id": "call_fixture", "content": "{\"found\":true}" })
    );
    let encoded = sent.body.to_string();
    assert!(!encoded.contains("continuation_id"));
    assert!(!encoded.contains("connection_id"));
    assert!(!encoded.contains("fixture-api-key"));
  }
  assert_eq!(
    requests[2].body["messages"][3]["reasoning_content"],
    "Second turn reasoning."
  );
}

#[tokio::test]
async fn continuation_rejects_wrong_session_mutation_and_connection_changes_before_network() {
  let server = Server::start(vec![Reply::Json(tool_response())]).await;
  let service = LlmService::new().unwrap();
  service.register_session("owner").unwrap();
  service.register_session("other").unwrap();
  let (first, _) = generate(&service, "owner", input("first"), &server.config).await;
  let continuation = followup(&first);
  let mut attempts = vec![("other", continuation.clone(), server.config.clone())];
  let mut invalid = continuation.clone();
  invalid.messages[1].continuation_id = Some("unknown-token".into());
  attempts.push(("owner", invalid, server.config.clone()));
  let mut changed = continuation.clone();
  changed.messages[1].content = "Altered assistant text.".into();
  attempts.push(("owner", changed, server.config.clone()));
  let mut changed = continuation.clone();
  changed.messages[1].tool_calls[0].arguments = "{}".into();
  attempts.push(("owner", changed, server.config.clone()));
  let mut connection = server.config.clone();
  connection.connection_id = "different-connection".into();
  attempts.push(("owner", continuation.clone(), connection));
  let mut model = server.config.clone();
  model.model_id = "different-model".into();
  attempts.push(("owner", continuation.clone(), model));
  let mut endpoint = server.config.clone();
  endpoint.base_url.push_str("/different");
  attempts.push(("owner", continuation, endpoint));
  for (index, (session_id, mut request, config)) in attempts.into_iter().enumerate() {
    request.request_id = format!("invalid_{index}");
    let (result, _) = generate(&service, session_id, request, &config).await;
    assert_eq!(result.status, LlmStatus::Failed);
    assert_eq!(result.error.as_ref().unwrap().code, "invalid_continuation");
    assert!(result.provenance.model_id.is_none());
    assert_eq!(
      server.requests().len(),
      1,
      "Invalid continuation reached the provider."
    );
  }
  // A rejected replay must not consume or corrupt the owner's token.
  // The next valid replay reaches our server, which intentionally returns an error.
  let (valid, _) = generate(&service, "owner", followup(&first), &server.config).await;
  assert_ne!(
    valid.error.as_ref().map(|error| error.code.as_str()),
    Some("invalid_continuation")
  );
  assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn cancellation_returns_native_partial_answer_and_actual_model() {
  let server = Server::start(vec![Reply::HoldStream(sse(json!({
    "model": "actual-model-revision",
    "choices": [{ "index": 0, "delta": { "content": "Visible partial." }, "finish_reason": null }]
  })))])
  .await;
  let service = Arc::new(LlmService::new().unwrap());
  service.register_session("session").unwrap();
  let cancel = service.clone();
  let events = Arc::new(Mutex::new(Vec::new()));
  let emitted = events.clone();
  let result = tokio::time::timeout(
    DEADLINE,
    service.generate(
      "session",
      input("cancelled"),
      server.config.clone(),
      None,
      move |event| {
        if event.kind == "delta" {
          cancel.cancel("session", "cancelled").unwrap();
        }
        emitted.lock().unwrap().push(event);
      },
    ),
  )
  .await
  .unwrap();
  assert_eq!(result.status, LlmStatus::Cancelled);
  assert_eq!(result.content, "Visible partial.");
  assert_eq!(
    result.provenance.model_id.as_deref(),
    Some("actual-model-revision")
  );
  assert_eq!(result.provenance.connection_id, server.config.connection_id);
  assert!(result.continuation_id.is_none());
  let events = events.lock().unwrap();
  let terminals: Vec<_> = events
    .iter()
    .filter(|event| event.kind == "terminal")
    .collect();
  assert_eq!(terminals.len(), 1);
  assert_eq!(
    terminals[0].result.as_ref().unwrap().content,
    result.content
  );
  assert_eq!(
    terminals[0].result.as_ref().unwrap().status,
    LlmStatus::Cancelled
  );
}

#[tokio::test]
async fn truncation_and_transport_eof_keep_partial_answers_without_claiming_completion() {
  let truncated = completion(
    json!({ "role": "assistant", "content": "Token-limited partial." }),
    "length",
  );
  let interrupted = sse(
    json!({ "model": "stream-model", "choices": [{ "index": 0, "delta": { "content": "Transport partial." }, "finish_reason": null }] }),
  );
  let server = Server::start(vec![Reply::Json(truncated), Reply::Stream(interrupted)]).await;
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let (truncated, _) = generate(&service, "session", input("truncated"), &server.config).await;
  assert_eq!(truncated.status, LlmStatus::Truncated);
  assert_eq!(truncated.content, "Token-limited partial.");
  assert_eq!(truncated.finish_reason.as_deref(), Some("length"));
  assert!(truncated.continuation_id.is_none());
  let (interrupted, _) = generate(&service, "session", input("interrupted"), &server.config).await;
  assert_eq!(interrupted.status, LlmStatus::Failed);
  assert_eq!(interrupted.content, "Transport partial.");
  assert_eq!(
    interrupted.provenance.model_id.as_deref(),
    Some("stream-model")
  );
  assert!(interrupted.error.is_some());
  assert!(interrupted.continuation_id.is_none());
}

#[tokio::test]
async fn model_provenance_is_not_invented_when_the_provider_omits_it() {
  let mut response = completion(json!({ "role": "assistant", "content": "Answer." }), "stop");
  response.as_object_mut().unwrap().remove("model");
  let server = Server::start(vec![Reply::Json(response)]).await;
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let (result, _) = generate(&service, "session", input("no_model"), &server.config).await;
  assert_eq!(result.status, LlmStatus::Completed);
  assert_eq!(result.provenance.requested_model_id, "requested-alias");
  assert_eq!(result.provenance.connection_id, server.config.connection_id);
  assert!(result.provenance.model_id.is_none());
}

#[tokio::test]
async fn explicit_unsupported_parameters_are_distinct_from_generic_provider_rejections() {
  let server = Server::start(vec![
    Reply::HttpError(
      400,
      json!({ "error": {
        "code": "unsupported_parameter",
        "param": "tool_choice",
        "message": "fixture-private-provider-detail"
      }}),
    ),
    Reply::HttpError(
      400,
      json!({ "error": {
        "code": "invalid_request_error",
        "message": "fixture-private-provider-detail"
      }}),
    ),
  ])
  .await;
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let mut request = input("unsupported");
  request.tool_choice = Some(LlmToolChoice::Mode("required".into()));
  let (unsupported, _) = generate(&service, "session", request.clone(), &server.config).await;
  assert_eq!(unsupported.status, LlmStatus::Unsupported);
  assert_eq!(
    unsupported.error.as_ref().unwrap().capability.as_deref(),
    Some("required_tool_choice")
  );
  request.request_id = "invalid_request".into();
  let (rejected, _) = generate(&service, "session", request, &server.config).await;
  assert_eq!(rejected.status, LlmStatus::Failed);
  assert_eq!(rejected.error.as_ref().unwrap().code, "provider_error");
  assert!(rejected.error.as_ref().unwrap().capability.is_none());
  for result in [unsupported, rejected] {
    let encoded = serde_json::to_string(&result).unwrap();
    assert!(!encoded.contains("fixture-private-provider-detail"));
    assert!(!encoded.contains("fixture-api-key"));
    assert!(result.provenance.model_id.is_none());
    assert_eq!(result.provenance.connection_id, server.config.connection_id);
  }
  assert!(server
    .requests()
    .iter()
    .all(|request| request.body["tool_choice"] == "required"));
}

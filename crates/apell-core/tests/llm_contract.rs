use apell_core::llm::{
  LlmConfig, LlmEvent, LlmRequest, LlmResult, LlmService, LlmStatus, LlmToolChoice,
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
  Redirect(String),
  TruncatedHttpError(u16),
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
        if let Reply::Redirect(location) = &reply {
          stream
            .write_all(
              format!("HTTP/1.1 307 Test\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .as_bytes(),
            )
            .await
            .unwrap();
          continue;
        }
        if let Reply::TruncatedHttpError(status) = &reply {
          stream
            .write_all(
              format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n{{\"error\":")
                .as_bytes(),
            )
            .await
            .unwrap();
          continue;
        }
        let (status, content_type, body, hold) = match reply {
          Reply::Json(body) => (200, "application/json", body.to_string(), false),
          Reply::HttpError(status, body) => (status, "application/json", body.to_string(), false),
          Reply::Stream(body) => (200, "text/event-stream", body, false),
          Reply::HoldStream(body) => (200, "text/event-stream", body, true),
          Reply::Redirect(_) | Reply::TruncatedHttpError(_) => unreachable!(),
        };
        let length = if hold {
          String::new()
        } else {
          format!("Content-Length: {}\r\n", body.len())
        };
        let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\n{length}Connection: close\r\n\r\n{body}");
        // Bounded reads and cancellation can close before the fixture finishes.
        if stream.write_all(response.as_bytes()).await.is_err() {
          continue;
        }
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
    messages: vec![json!({"role":"user", "content":"Check the fixture.", "client_metadata":{"retain":true}})],
    tools: vec![serde_json::from_value(json!({
      "name": "inspect_fixture",
      "description": "Inspect a local fixture.",
      "parameters": { "type": "object", "properties": { "label": { "type": "string" } }, "required": ["label"] }
    })).unwrap()],
    ..LlmRequest::default()
  }
}

fn assistant(result: &LlmResult) -> Value {
  result
    .message
    .clone()
    .expect("Successful results must carry a replayable assistant message.")
}

fn followup(result: &LlmResult) -> LlmRequest {
  let mut request = input("followup");
  request.messages.push(assistant(result));
  request.messages.push(json!({
    "role":"tool", "content":"{\"found\":true}", "tool_call_id":"call_fixture",
    "client_metadata":{"source":"applet"}
  }));
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
  // The same message must fit the SDK's whole-request depth limit on replay:
  // request -> messages -> assistant -> 61 containers -> scalar at depth 64.
  let mut deep_metadata = Value::Null;
  for _ in 0..61 {
    deep_metadata = json!([deep_metadata]);
  }
  completion(
    json!({
      "role": "assistant",
      "content": null,
      "reasoning_content": "Visible provider reasoning.",
      "reasoning_details": [{ "type": "reasoning.encrypted", "data": "opaque-provider-state", "id": "reasoning_fixture", "index": 0 }],
      "tool_calls": [{ "id": "call_fixture", "type": "function", "function": { "name": "inspect_fixture", "arguments": "{\"label\":\"例\"}", "function_metadata":{"keep":true} }, "extra_content":{"google":{"thought_signature":"opaque-signature"}} }],
      "arbitrary_metadata":{"nested":[1,null,{"signature":"opaque"}]},
      "deep_metadata":deep_metadata,
      "annotations":[{"source":"fixture"}]
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
  assert!(events.iter().all(|event| matches!(
    event.kind.as_str(),
    "started" | "text_delta" | "reasoning_delta"
  )));
  assert_eq!(
    result.message.is_some(),
    matches!(result.status, LlmStatus::Completed | LlmStatus::ToolCalls)
  );
  assert!(!serde_json::to_value(&result)
    .unwrap()
    .as_object()
    .unwrap()
    .contains_key("continuation_id"));
  (result, events)
}

#[tokio::test]
async fn applet_owned_messages_replay_exactly_after_session_close_and_service_recreation() {
  let second_message = json!({
    "role":"assistant", "content":"The fixture exists.",
    "reasoning_content":"Second turn reasoning.", "provider_extension":{"ref":"response-reference"}
  });
  let server = Server::start(vec![
    Reply::Json(tool_response()),
    Reply::Json(completion(second_message.clone(), "stop")),
    Reply::Json(completion(
      json!({"role":"assistant", "content":"Still available."}),
      "stop",
    )),
  ])
  .await;
  let service = LlmService::new().unwrap();
  service.register_session("first_launch").unwrap();
  let mut request = input("tools");
  request.stream = false;
  request.max_output_tokens = Some(1024);
  request.tool_choice = Some(LlmToolChoice::Named {
    name: "inspect_fixture".into(),
  });
  let (first, events) = generate(&service, "first_launch", request, &server.config).await;
  assert_eq!(first.status, LlmStatus::ToolCalls);
  assert_eq!(
    first.message.as_ref().unwrap(),
    &tool_response()["choices"][0]["message"]
  );
  assert_eq!(first.tool_calls[0].arguments, "{\"label\":\"例\"}");
  assert!(events.iter().all(|event| event.kind == "started"));
  assert_eq!(first.provenance.connection_id, server.config.connection_id);
  assert_eq!(first.provenance.requested_model_id, "requested-alias");
  assert_eq!(
    first.provenance.model_id.as_deref(),
    Some("actual-model-revision")
  );
  // This serialization represents applet persistence. No host cache, launch
  // identity, or original service is available when it is replayed.
  let persisted = serde_json::to_string(&followup(&first)).unwrap();
  service.close_session("first_launch");
  drop(service);
  let service = LlmService::new().unwrap();
  service.register_session("second_launch").unwrap();
  let mut next: LlmRequest = serde_json::from_str(&persisted).unwrap();
  next.stream = false;
  next.tool_choice = Some(LlmToolChoice::Mode("auto".into()));
  let (second, _) = generate(&service, "second_launch", next.clone(), &server.config).await;
  assert_eq!(second.status, LlmStatus::Completed);
  assert_eq!(second.message, Some(second_message.clone()));
  next.request_id = "third_turn".into();
  next.messages.push(assistant(&second));
  next
    .messages
    .push(json!({"role":"user", "content":"Check again."}));
  let (third, _) = generate(&service, "second_launch", next, &server.config).await;
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
  assert_eq!(requests[0].body["stream"], false);
  assert_eq!(requests[0].body["max_tokens"], 1024);
  assert!(requests[0].body.get("max_output_tokens").is_none());
  assert_eq!(requests[0].body["tools"][0]["type"], "function");
  assert_eq!(
    requests[0].body["tool_choice"],
    json!({"type":"function", "function":{"name":"inspect_fixture"}})
  );
  assert_eq!(requests[1].body["tool_choice"], "auto");
  for sent in &requests[1..] {
    assert_eq!(sent.body["messages"][0], input("fixture").messages[0]);
    assert_eq!(
      sent.body["messages"][1],
      tool_response()["choices"][0]["message"]
    );
    assert_eq!(
      sent.body["messages"][2],
      json!({
        "role":"tool", "tool_call_id":"call_fixture", "content":"{\"found\":true}",
        "client_metadata":{"source":"applet"}
      })
    );
    let encoded = sent.body.to_string();
    assert!(!encoded.contains("continuation_id"));
    assert!(!encoded.contains("connection_id"));
    assert!(!encoded.contains("fixture-api-key"));
  }
  assert_eq!(requests[2].body["messages"][3], second_message);
}

#[tokio::test]
async fn unknown_stream_metadata_requires_explicit_buffered_request_without_hidden_retry() {
  let message = json!({
    "role":"assistant", "content":"Answer.",
    "reasoning_details":[{"type":"reasoning.encrypted", "data":"opaque"}],
    "vendor_metadata":{"signature":"signed"}
  });
  let server = Server::start(vec![
    Reply::Stream(sse(
      json!({"choices":[{"delta":message, "finish_reason":"stop"}]}),
    )),
    Reply::Json(completion(message.clone(), "stop")),
  ])
  .await;
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let (streamed, _) = generate(&service, "session", input("stream"), &server.config).await;
  assert_eq!(streamed.status, LlmStatus::Unsupported);
  assert_eq!(streamed.content, "Answer.");
  assert!(streamed.message.is_none());
  assert_eq!(
    streamed.error.unwrap().capability.as_deref(),
    Some("stream_metadata")
  );
  assert_eq!(server.requests().len(), 1);
  let mut request = input("buffered");
  request.stream = false;
  let (buffered, events) = generate(&service, "session", request, &server.config).await;
  assert_eq!(buffered.status, LlmStatus::Completed);
  assert_eq!(buffered.message, Some(message));
  assert!(events.iter().all(|event| event.kind == "started"));
  assert_eq!(server.requests()[1].body["stream"], false);
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
        if event.kind == "text_delta" {
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
  assert!(result.message.is_none());
  let events = events.lock().unwrap();
  assert!(events.iter().all(|event| matches!(
    event.kind.as_str(),
    "started" | "text_delta" | "reasoning_delta"
  )));
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
  assert!(truncated.message.is_none());
  let (interrupted, _) = generate(&service, "session", input("interrupted"), &server.config).await;
  assert_eq!(interrupted.status, LlmStatus::Failed);
  assert_eq!(interrupted.content, "Transport partial.");
  assert_eq!(
    interrupted.provenance.model_id.as_deref(),
    Some("stream-model")
  );
  assert!(interrupted.error.is_some());
  assert!(interrupted.message.is_none());
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

#[tokio::test]
async fn tokn_preserves_keyless_localhost_prefix_and_qualified_model_ids() {
  let mut server = Server::start(vec![Reply::Json(completion(
    json!({ "role": "assistant", "content": "Keyless answer." }),
    "stop",
  ))])
  .await;
  server.config.base_url = server.config.base_url.replace("127.0.0.1", "localhost");
  server.config.base_url.push_str("/nested/");
  server.config.model_id = "organization/model-revision".into();
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let result = tokio::time::timeout(
    DEADLINE,
    service.generate(
      "session",
      input("keyless"),
      server.config.clone(),
      None,
      |_| {},
    ),
  )
  .await
  .unwrap();
  assert_eq!(result.status, LlmStatus::Completed, "{:?}", result.error);
  assert_eq!(
    result.provenance.requested_model_id,
    "organization/model-revision"
  );
  let requests = server.requests();
  assert_eq!(requests.len(), 1);
  assert!(requests[0]
    .headers
    .starts_with("POST /v1/nested/chat/completions HTTP/1.1\r\n"));
  assert_eq!(requests[0].body["model"], "organization/model-revision");
  assert_eq!(requests[0].body["stream"], true);
  let headers = requests[0].headers.to_ascii_lowercase();
  assert!(!headers.contains("authorization:"));
  assert!(!headers.contains("x-api-key:"));
  assert!(!headers.contains("x-project-cwd:"));
  assert!(!headers.contains("x-tokn-"));
  assert!(headers.contains("accept-encoding: identity\r\n"));
}

#[tokio::test]
async fn tokn_does_not_retry_rate_limits_or_server_errors() {
  let server = Server::start(vec![
    Reply::HttpError(
      429,
      json!({ "error": { "message": "private-rate-detail" } }),
    ),
    Reply::HttpError(
      503,
      json!({ "error": { "message": "private-service-detail" } }),
    ),
  ])
  .await;
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  for (index, status) in [429, 503].into_iter().enumerate() {
    let (result, _) = generate(
      &service,
      "session",
      input(&format!("failure_{index}")),
      &server.config,
    )
    .await;
    assert_eq!(result.status, LlmStatus::Failed);
    let error = result.error.unwrap();
    assert_eq!(error.code, "provider_error");
    assert!(error.retryable);
    assert_eq!(
      error.message,
      format!("The LLM provider returned HTTP {status}.")
    );
    assert_eq!(server.requests().len(), index + 1);
  }
}

#[tokio::test]
async fn tokn_does_not_forward_credentials_to_redirect_targets() {
  let destination = Server::start(vec![]).await;
  let server = Server::start(vec![Reply::Redirect(format!(
    "{}/chat/completions",
    destination.config.base_url
  ))])
  .await;
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let (result, _) = generate(&service, "session", input("redirect"), &server.config).await;
  assert_eq!(result.status, LlmStatus::Failed);
  assert_eq!(
    result.error.unwrap().message,
    "The LLM provider returned HTTP 307."
  );
  assert_eq!(server.requests().len(), 1);
  assert!(destination.requests().is_empty());
}

#[tokio::test]
async fn tokn_preserves_stream_errors_and_explicit_finish_without_done_marker() {
  let partial = sse(json!({
    "model": "stream-model",
    "choices": [{ "index": 0, "delta": { "content": "Partial answer." }, "finish_reason": null }]
  }));
  let unsupported = sse(json!({ "error": {
    "code": "unsupported_parameter", "param": "tools", "message": "private-stream-detail"
  }}));
  let finished = sse(json!({
    "model": "stream-model",
    "choices": [{ "index": 0, "delta": { "content": "Finished answer." }, "finish_reason": "stop" }]
  }));
  let server = Server::start(vec![
    Reply::Stream(partial + &unsupported),
    Reply::Stream(finished),
  ])
  .await;
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let (failed, _) = generate(&service, "session", input("stream_error"), &server.config).await;
  assert_eq!(failed.status, LlmStatus::Unsupported);
  assert_eq!(failed.content, "Partial answer.");
  assert_eq!(failed.error.unwrap().capability.as_deref(), Some("tools"));
  let (finished, _) = generate(&service, "session", input("finished"), &server.config).await;
  assert_eq!(finished.status, LlmStatus::Completed);
  assert_eq!(finished.content, "Finished answer.");
  assert_eq!(
    finished.provenance.model_id.as_deref(),
    Some("stream-model")
  );
}

#[tokio::test]
async fn tokn_bounds_cumulative_stream_bytes_even_when_events_are_only_comments() {
  let partial = sse(json!({
    "model": "stream-model",
    "choices": [{ "index": 0, "delta": { "content": "Bounded partial." }, "finish_reason": null }]
  }));
  let server = Server::start(vec![Reply::Stream(
    partial + &": ignored\n\n".repeat(900_000),
  )])
  .await;
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let (result, _) = generate(&service, "session", input("too_large"), &server.config).await;
  assert_eq!(result.status, LlmStatus::Failed);
  assert_eq!(result.content, "Bounded partial.");
  assert_eq!(result.error.unwrap().code, "response_limit");
  assert!(result.message.is_none());
}

#[tokio::test]
async fn invalid_credentials_fail_before_network_instead_of_becoming_keyless() {
  let server = Server::start(vec![]).await;
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let result = tokio::time::timeout(
    DEADLINE,
    service.generate(
      "session",
      input("invalid_key"),
      server.config.clone(),
      Some("private-invalid-key\r\nx-extra: forbidden".into()),
      |_| {},
    ),
  )
  .await
  .unwrap();
  assert_eq!(result.status, LlmStatus::Failed);
  assert!(server.requests().is_empty());
  assert!(!serde_json::to_string(&result)
    .unwrap()
    .contains("private-invalid-key"));
}

#[tokio::test]
async fn http_failures_keep_status_when_error_bodies_are_oversized_or_interrupted() {
  let server = Server::start(vec![
    Reply::HttpError(
      429,
      json!({ "error": { "message": "x".repeat(9 * 1024 * 1024) } }),
    ),
    Reply::TruncatedHttpError(400),
  ])
  .await;
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  for (index, status) in [429, 400].into_iter().enumerate() {
    let (result, _) = generate(
      &service,
      "session",
      input(&format!("error_body_{index}")),
      &server.config,
    )
    .await;
    assert_eq!(result.status, LlmStatus::Failed);
    let error = result.error.unwrap();
    assert_eq!(error.code, "provider_error");
    assert_eq!(
      error.message,
      format!("The LLM provider returned HTTP {status}.")
    );
    assert_eq!(error.retryable, status == 429);
  }
  assert_eq!(server.requests().len(), 2);
}

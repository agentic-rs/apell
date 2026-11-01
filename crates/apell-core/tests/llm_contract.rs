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
      provider_id: "custom".into(),
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

fn account(config: &LlmConfig, token: &str) -> tokn_sdk::account::AccountConfig {
  serde_json::from_value(json!({
    "id":config.connection_id,
    "provider":if config.provider_id == "custom" { "openai" } else { &config.provider_id },
    "api_key":token,
    "provider_account_id":"fixture-account"
  }))
  .unwrap()
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
      Some(account(config, "fixture-api-key")),
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
      Some(account(
        &server.config,
        "private-invalid-key\r\nx-extra: forbidden",
      )),
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

fn native_input(request_id: &str) -> LlmRequest {
  let mut request = input(request_id);
  request.messages = vec![json!({"role":"user", "content":"Check the fixture."})];
  request
}

fn native_followup(result: &LlmResult) -> LlmRequest {
  let mut request = native_input("native_followup");
  request.messages.push(assistant(result));
  request
    .messages
    .push(json!({"role":"tool", "content":"{\"found\":true}", "tool_call_id":"call_fixture"}));
  request
}

fn responses(output: Value, status: &str) -> Value {
  json!({"id":"response_fixture", "model":"actual-model-revision", "status":status, "output":output})
}

fn response_text(text: &str) -> Value {
  json!({"type":"message", "id":"message_fixture", "role":"assistant", "status":"completed", "content":[{
    "type":"output_text", "text":text, "annotations":[{"type":"fixture", "nested":{"retain":true}}]
  }]})
}

fn response_tools() -> Value {
  json!([
    {"type":"reasoning", "id":"reasoning_fixture", "encrypted_content":"encrypted-reasoning", "summary":[{"type":"summary_text", "text":"Plan."}], "vendor":{"retain":true}},
    {"type":"function_call", "id":"function_item", "call_id":"call_fixture", "name":"inspect_fixture", "arguments":"{\"label\":\"fixture\"}", "status":"completed", "vendor_signature":"signed"}
  ])
}

#[tokio::test]
async fn codex_uses_native_responses_and_replays_client_state_after_service_recreation() {
  let output = response_tools();
  let first = sse(json!({"type":"response.reasoning_summary_text.delta", "delta":"Plan."}))
    + &sse(json!({"type":"response.completed", "response":responses(output.clone(), "completed")}));
  let second = sse(
    json!({"type":"response.completed", "response":responses(json!([response_text("Found.")]), "completed")}),
  );
  let mut server = Server::start(vec![Reply::Stream(first), Reply::Stream(second)]).await;
  server.config.provider_id = "codex".into();
  let service = LlmService::new().unwrap();
  service.register_session("first_launch").unwrap();
  let (first, events) = generate(
    &service,
    "first_launch",
    native_input("native_tools"),
    &server.config,
  )
  .await;
  assert_eq!(first.status, LlmStatus::ToolCalls, "{:?}", first.error);
  assert_eq!(first.tool_calls[0].id, "call_fixture");
  assert_eq!(
    first.message.as_ref().unwrap()["provider_state"]["output"],
    output
  );
  assert_eq!(
    events
      .iter()
      .filter(|event| event.kind == "reasoning_delta")
      .map(|event| event.text.as_str())
      .collect::<String>(),
    "Plan."
  );
  let persisted = serde_json::to_string(&native_followup(&first)).unwrap();
  service.close_session("first_launch");
  drop(service);
  let service = LlmService::new().unwrap();
  service.register_session("reopened").unwrap();
  let mut followup: LlmRequest = serde_json::from_str(&persisted).unwrap();
  followup.stream = false;
  let (second, events) = generate(&service, "reopened", followup, &server.config).await;
  assert_eq!(second.status, LlmStatus::Completed, "{:?}", second.error);
  assert_eq!(second.content, "Found.");
  assert!(events.iter().all(|event| event.kind == "started"));
  let sent = server.requests();
  assert_eq!(sent.len(), 2);
  for request in &sent {
    assert!(request.headers.starts_with("POST /v1/responses HTTP/1.1"));
    let headers = request.headers.to_ascii_lowercase();
    assert!(headers.contains("authorization: bearer fixture-api-key"));
    assert!(headers.contains("chatgpt-account-id: fixture-account"));
    assert!(headers.contains("openai-beta:"));
    assert_eq!(request.body["stream"], true);
    assert_eq!(request.body["store"], false);
    assert_eq!(
      request.body["include"],
      json!(["reasoning.encrypted_content"])
    );
    assert!(!request.body.to_string().contains("provider_state"));
    assert!(!request.body.to_string().contains("fixture-api-key"));
  }
  assert_eq!(sent[1].body["input"][1], output[0]);
  assert_eq!(sent[1].body["input"][2], output[1]);
  assert_eq!(sent[1].body["input"][3]["type"], "function_call_output");
  assert_eq!(sent[1].body["input"][3]["call_id"], "call_fixture");
}

#[tokio::test]
async fn openai_responses_buffered_output_and_max_tokens_keep_native_fields() {
  let output = json!([response_text("Native answer.")]);
  let mut server = Server::start(vec![Reply::Json(responses(output.clone(), "completed"))]).await;
  server.config.provider_id = "openai".into();
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let mut input = native_input("native_buffered");
  input.stream = false;
  input.max_output_tokens = Some(2048);
  let (result, events) = generate(&service, "session", input, &server.config).await;
  assert_eq!(result.status, LlmStatus::Completed, "{:?}", result.error);
  assert_eq!(
    result.message.as_ref().unwrap()["provider_state"]["output"],
    output
  );
  assert_eq!(
    result.provenance.model_id.as_deref(),
    Some("actual-model-revision")
  );
  assert!(events.iter().all(|event| event.kind == "started"));
  let sent = server.requests();
  assert_eq!(sent[0].body["max_output_tokens"], 2048);
  assert!(sent[0].body.get("max_tokens").is_none());
  assert_eq!(sent[0].body["stream"], false);
}

fn messages_start() -> Value {
  json!({"type":"message_start", "message":{
    "id":"message_fixture", "type":"message", "model":"actual-model-revision", "role":"assistant", "content":[],
    "stop_reason":null, "stop_sequence":null, "usage":{"input_tokens":8, "output_tokens":1}
  }})
}

fn messages_tool_stream() -> String {
  [
    messages_start(),
    json!({"type":"content_block_start", "index":0, "content_block":{"type":"thinking", "thinking":"", "signature":""}}),
    json!({"type":"content_block_delta", "index":0, "delta":{"type":"thinking_delta", "thinking":"Plan."}}),
    json!({"type":"content_block_delta", "index":0, "delta":{"type":"signature_delta", "signature":"sig"}}),
    json!({"type":"content_block_delta", "index":0, "delta":{"type":"signature_delta", "signature":"ned"}}),
    json!({"type":"content_block_stop", "index":0}),
    json!({"type":"content_block_start", "index":1, "content_block":{"type":"tool_use", "id":"call_fixture", "name":"inspect_fixture", "input":{}, "vendor_signature":"tool-signature"}}),
    json!({"type":"content_block_delta", "index":1, "delta":{"type":"input_json_delta", "partial_json":"{\"label\":"}}),
    json!({"type":"content_block_delta", "index":1, "delta":{"type":"input_json_delta", "partial_json":"\"fixture\"}"}}),
    json!({"type":"content_block_stop", "index":1}),
    json!({"type":"message_delta", "delta":{"stop_reason":"tool_use", "stop_sequence":null}, "usage":{"output_tokens":5}}),
    json!({"type":"message_stop"}),
  ].into_iter().map(sse).collect()
}

#[tokio::test]
async fn copilot_messages_preserve_signed_blocks_and_tool_results_without_host_state() {
  let final_message = json!({"id":"final_message", "type":"message", "model":"actual-model-revision", "role":"assistant", "content":[{"type":"text", "text":"Found."}], "stop_reason":"end_turn", "usage":{"output_tokens":2}});
  let mut server = Server::start(vec![
    Reply::Stream(messages_tool_stream()),
    Reply::Json(final_message.clone()),
  ])
  .await;
  server.config.provider_id = "github-copilot".into();
  server.config.model_id = "claude-fixture".into();
  server.config.base_url = server.config.base_url.trim_end_matches("/v1").to_owned();
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let (first, events) = generate(
    &service,
    "session",
    native_input("messages_tools"),
    &server.config,
  )
  .await;
  assert_eq!(first.status, LlmStatus::ToolCalls, "{:?}", first.error);
  let message = first.message.as_ref().unwrap();
  assert_eq!(message["provider_state"]["protocol"], "messages");
  let native = &message["provider_state"]["output"][0];
  assert_eq!(native["content"][0]["signature"], "signed");
  assert_eq!(native["content"][1]["input"], json!({"label":"fixture"}));
  assert_eq!(native["content"][1]["vendor_signature"], "tool-signature");
  assert_eq!(
    events
      .iter()
      .filter(|event| event.kind == "reasoning_delta")
      .map(|event| event.text.as_str())
      .collect::<String>(),
    "Plan."
  );
  let (second, _) = generate(&service, "session", native_followup(&first), &server.config).await;
  assert_eq!(second.status, LlmStatus::Completed, "{:?}", second.error);
  assert_eq!(
    second.message.as_ref().unwrap()["provider_state"]["output"][0],
    final_message
  );
  let sent = server.requests();
  for request in &sent {
    assert!(request.headers.starts_with("POST /v1/messages HTTP/1.1"));
    let headers = request.headers.to_ascii_lowercase();
    assert!(headers.contains("editor-version:"));
    assert!(headers.contains("editor-plugin-version:"));
    assert!(headers.contains("copilot-integration-id:"));
    assert!(!request.body.to_string().contains("provider_state"));
  }
  assert!(sent[0]
    .headers
    .to_ascii_lowercase()
    .contains("x-initiator: user"));
  assert!(sent[1]
    .headers
    .to_ascii_lowercase()
    .contains("x-initiator: agent"));
  assert_eq!(
    sent[1].body["messages"][1],
    json!({"role":"assistant", "content":native["content"]})
  );
  assert_eq!(
    sent[1].body["messages"][2]["content"][0]["type"],
    "tool_result"
  );
}

#[tokio::test]
async fn native_state_binding_and_display_mutation_fail_before_network() {
  let mut server = Server::start(vec![Reply::Json(responses(response_tools(), "completed"))]).await;
  server.config.provider_id = "openai".into();
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let (first, _) = generate(&service, "session", native_input("initial"), &server.config).await;
  assert_eq!(first.status, LlmStatus::ToolCalls);
  for variant in 0..6 {
    let mut config = server.config.clone();
    let mut input = native_followup(&first);
    input.request_id = format!("mismatch_{variant}");
    match variant {
      0 => config.connection_id = "other-connection".into(),
      1 => config.model_id = "other-model".into(),
      2 => config.provider_id = "custom".into(),
      3 => input.messages[1]["content"] = json!("changed"),
      4 => input.messages[1]["tool_calls"][0]["function"]["arguments"] = json!("{}"),
      _ => input.messages[1]["provider_state"]["protocol"] = json!("messages"),
    }
    let (result, _) = generate(&service, "session", input, &config).await;
    assert_eq!(result.status, LlmStatus::Failed);
    assert_eq!(result.error.unwrap().code, "provider_state_mismatch");
    assert!(result.message.is_none());
    assert_eq!(server.requests().len(), 1);
  }
}

#[tokio::test]
async fn native_stream_cancellation_preserves_partial_text_and_provenance() {
  for (provider, model, stream) in [
    (
      "openai",
      "fixture",
      sse(
        json!({"type":"response.created", "response":{"id":"response_fixture", "model":"actual-model-revision"}}),
      ) + &sse(json!({"type":"response.output_text.delta", "delta":"Partial."})),
    ),
    (
      "github-copilot",
      "claude-fixture",
      sse(messages_start())
        + &sse(
          json!({"type":"content_block_start", "index":0, "content_block":{"type":"text", "text":"Partial."}}),
        ),
    ),
  ] {
    let mut server = Server::start(vec![Reply::HoldStream(stream)]).await;
    server.config.provider_id = provider.into();
    server.config.model_id = model.into();
    let service = Arc::new(LlmService::new().unwrap());
    service.register_session("session").unwrap();
    let cancel = service.clone();
    let result = tokio::time::timeout(
      DEADLINE,
      service.generate(
        "session",
        native_input("cancel"),
        server.config.clone(),
        Some(account(&server.config, "fixture-key")),
        move |event| {
          if event.kind == "text_delta" {
            cancel.cancel("session", "cancel").unwrap();
          }
        },
      ),
    )
    .await
    .unwrap();
    assert_eq!(result.status, LlmStatus::Cancelled);
    assert_eq!(result.content, "Partial.");
    assert_eq!(
      result.provenance.model_id.as_deref(),
      Some("actual-model-revision")
    );
    assert!(result.message.is_none());
    assert!(result.tool_calls.is_empty());
  }
}

#[tokio::test]
async fn native_responses_distinguish_truncation_refusal_incomplete_items_and_limits() {
  let mut truncated = responses(
    json!([response_text("Partial."), {"type":"function_call", "arguments":"{"}]),
    "incomplete",
  );
  truncated["incomplete_details"] = json!({"reason":"max_output_tokens"});
  let refusal = responses(
    json!([{"type":"message", "role":"assistant", "content":[{"type":"refusal", "refusal":"Declined."}]}]),
    "completed",
  );
  let incomplete = responses(
    json!([{"type":"function_call", "call_id":"call_fixture", "name":"inspect_fixture", "arguments":"{", "status":"in_progress"}]),
    "completed",
  );
  let oversized = sse(json!({"type":"response.output_text.delta", "delta":"Bounded partial."}))
    + &sse(
      json!({"type":"response.completed", "response":responses(json!([response_text("Bounded partial."), {"type":"reasoning", "encrypted_content":"x".repeat(1024*1024)}]), "completed")}),
    );
  let mut server = Server::start(vec![
    Reply::Json(truncated),
    Reply::Json(refusal),
    Reply::Json(incomplete),
    Reply::Stream(oversized),
    Reply::Stream(sse(
      json!({"type":"response.output_text.delta", "delta":"Interrupted."}),
    )),
    Reply::Json(responses(json!([response_text("Buffered partial."), {"type":"reasoning","encrypted_content":"x".repeat(1024*1024)}]),"completed")),
  ])
  .await;
  server.config.provider_id = "openai".into();
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  for (index, status, code, content) in [
    (0, LlmStatus::Truncated, "truncated", "Partial."),
    (1, LlmStatus::Failed, "refusal", "Declined."),
    (2, LlmStatus::Failed, "invalid_response", ""),
    (3, LlmStatus::Failed, "response_limit", "Bounded partial."),
    (4, LlmStatus::Failed, "incomplete_response", "Interrupted."),
    (5, LlmStatus::Failed, "response_limit", "Buffered partial."),
  ] {
    let (result, _) = generate(
      &service,
      "session",
      native_input(&format!("outcome_{index}")),
      &server.config,
    )
    .await;
    assert_eq!(result.status, status, "{:?}", result.error);
    assert_eq!(result.error.unwrap().code, code);
    assert_eq!(result.content, content);
    assert!(result.message.is_none());
    assert!(result.tool_calls.is_empty());
  }
}

#[tokio::test]
async fn scoped_keyless_accounts_and_all_chat_provider_drivers_keep_raw_messages() {
  for provider in [
    "custom",
    "llama-cpp",
    "deepseek",
    "zai",
    "zai-coding-plan",
    "zhipuai",
    "zhipuai-coding-plan",
  ] {
    let mut server = Server::start(vec![Reply::Json(completion(
      json!({"role":"assistant", "content":"Ready."}),
      "stop",
    ))])
    .await;
    server.config.provider_id = provider.into();
    let mut credential = account(&server.config, "fixture-secret");
    if matches!(provider, "custom" | "llama-cpp") {
      credential.api_key = None;
    }
    let service = LlmService::new().unwrap();
    service.register_session("session").unwrap();
    let request = input("chat_driver");
    let result = service
      .generate(
        "session",
        request.clone(),
        server.config.clone(),
        Some(credential),
        |_| {},
      )
      .await;
    assert_eq!(
      result.status,
      LlmStatus::Completed,
      "{provider}: {:?}",
      result.error
    );
    let sent = server.requests();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].headers.starts_with("POST /v1/chat/completions "));
    assert_eq!(sent[0].body["messages"], json!(request.messages));
    assert_eq!(
      sent[0]
        .headers
        .to_ascii_lowercase()
        .contains("authorization:"),
      !matches!(provider, "custom" | "llama-cpp")
    );
  }
}

#[tokio::test]
async fn account_scope_and_unsupported_native_input_fail_before_network() {
  let mut server = Server::start(Vec::new()).await;
  server.config.provider_id = "codex".into();
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  for field in ["id", "provider", "enabled"] {
    let mut snapshot = serde_json::to_value(account(&server.config, "fixture-secret")).unwrap();
    snapshot[field] = if field == "enabled" {
      json!(false)
    } else {
      json!("another")
    };
    let result = service
      .generate(
        "session",
        native_input(field),
        server.config.clone(),
        Some(serde_json::from_value(snapshot).unwrap()),
        |_| {},
      )
      .await;
    assert_eq!(result.error.unwrap().code, "credential_mismatch");
  }
  let mut request = native_input("budget");
  request.max_output_tokens = Some(50);
  let (result, _) = generate(&service, "session", request, &server.config).await;
  assert_eq!(result.status, LlmStatus::Unsupported);
  assert_eq!(
    result.error.unwrap().capability.as_deref(),
    Some("max_output_tokens")
  );
  let (result, _) = generate(&service, "session", input("metadata"), &server.config).await;
  assert_eq!(result.status, LlmStatus::Unsupported);
  assert_eq!(
    result.error.unwrap().capability.as_deref(),
    Some("cross_protocol_metadata")
  );
  assert!(server.requests().is_empty());
}

#[tokio::test]
async fn native_messages_require_closed_blocks_and_reject_unknown_delta_merges() {
  let start = sse(messages_start())
    + &sse(
      json!({"type":"content_block_start", "index":0, "content_block":{"type":"text", "text":"Partial."}}),
    );
  let block_stop = sse(json!({"type":"content_block_stop", "index":0}));
  let text_delta = sse(
    json!({"type":"content_block_delta", "index":0, "delta":{"type":"text_delta", "text":"More."}}),
  );
  let finish = sse(
    json!({"type":"message_delta", "delta":{"stop_reason":"end_turn"}, "usage":{"output_tokens":1}}),
  ) + &sse(json!({"type":"message_stop"}));
  let unfinished_tool = start.clone()
    + &block_stop
    + &sse(
      json!({"type":"content_block_start", "index":1, "content_block":{"type":"tool_use", "id":"call_fixture", "name":"inspect_fixture", "input":{}}}),
    )
    + &sse(
      json!({"type":"content_block_delta", "index":1, "delta":{"type":"input_json_delta", "partial_json":"{"}}),
    )
    + &sse(json!({"type":"message_delta", "delta":{"stop_reason":"max_tokens"}}))
    + &sse(json!({"type":"message_stop"}));
  let mut server = Server::start(vec![
    Reply::Stream(start.clone() + &finish),
    Reply::Stream(start.clone() + &block_stop + &block_stop + &finish),
    Reply::Stream(start.clone() + &block_stop + &text_delta + &finish),
    Reply::Stream(start.clone() + &sse(json!({"type":"content_block_delta", "index":0, "delta":{"type":"text_delta", "text":"", "vendor_merge":{"signed":"opaque"}}}))),
    Reply::Stream(unfinished_tool),
    Reply::Stream(start),
  ]).await;
  server.config.provider_id = "github-copilot".into();
  server.config.model_id = "claude-fixture".into();
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  for (index, status, code) in [
    (0, LlmStatus::Failed, "invalid_response"),
    (1, LlmStatus::Failed, "invalid_response"),
    (2, LlmStatus::Failed, "invalid_response"),
    (3, LlmStatus::Unsupported, "unsupported_capability"),
    (4, LlmStatus::Truncated, "truncated"),
    (5, LlmStatus::Failed, "incomplete_response"),
  ] {
    let (result, _) = generate(
      &service,
      "session",
      native_input(&format!("lifecycle_{index}")),
      &server.config,
    )
    .await;
    assert_eq!(result.status, status, "{:?}", result.error);
    assert_eq!(result.error.unwrap().code, code);
    assert_eq!(result.content, "Partial.");
    assert!(result.message.is_none());
    assert!(result.tool_calls.is_empty());
  }
}

#[tokio::test]
async fn opencode_native_dispatch_uses_protocol_specific_auth_and_honors_tool_choice() {
  for (model, path, reply, choice) in [
    (
      "gpt-fixture",
      "responses",
      responses(response_tools(), "completed"),
      json!({"type":"function","name":"inspect_fixture"}),
    ),
    (
      "qwen-fixture",
      "messages",
      json!({"id":"message_fixture","role":"assistant","content":[{"type":"tool_use","id":"call_fixture","name":"inspect_fixture","input":{"label":"fixture"}}],"stop_reason":"tool_use"}),
      json!({"type":"tool","name":"inspect_fixture"}),
    ),
    (
      "other-fixture",
      "chat/completions",
      tool_response(),
      json!({"type":"function","function":{"name":"inspect_fixture"}}),
    ),
  ] {
    let mut server = Server::start(vec![Reply::Json(reply)]).await;
    server.config.provider_id = "opencode-go".into();
    server.config.model_id = model.into();
    let service = LlmService::new().unwrap();
    service.register_session("session").unwrap();
    let mut request = native_input("opencode");
    request.tool_choice = Some(LlmToolChoice::Named {
      name: "inspect_fixture".into(),
    });
    let (result, _) = generate(&service, "session", request, &server.config).await;
    assert_eq!(result.status, LlmStatus::ToolCalls, "{:?}", result.error);
    let sent = server.requests();
    assert!(sent[0].headers.starts_with(&format!("POST /v1/{path} ")));
    assert_eq!(sent[0].body["tool_choice"], choice);
    let headers = sent[0].headers.to_ascii_lowercase();
    assert_eq!(
      headers.contains("x-api-key: fixture-api-key"),
      path == "messages"
    );
    assert_eq!(
      headers.contains("authorization: bearer fixture-api-key"),
      path != "messages"
    );
    if path == "messages" {
      assert!(headers.contains("anthropic-version: 2023-06-01"));
    }
  }
}

#[tokio::test]
async fn native_messages_group_parallel_tool_results_into_the_immediate_user_turn() {
  let first = json!({"id":"message_fixture","role":"assistant","content":[
    {"type":"tool_use","id":"call_fixture","name":"inspect_fixture","input":{"label":"one"}},
    {"type":"tool_use","id":"call_second","name":"inspect_fixture","input":{"label":"two"}}
  ],"stop_reason":"tool_use"});
  let second = json!({"id":"message_second","role":"assistant","content":[{"type":"text","text":"Both found."}],"stop_reason":"end_turn"});
  let mut server = Server::start(vec![Reply::Json(first), Reply::Json(second)]).await;
  server.config.provider_id = "github-copilot".into();
  server.config.model_id = "claude-fixture".into();
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let (result, _) = generate(
    &service,
    "session",
    native_input("parallel"),
    &server.config,
  )
  .await;
  assert_eq!(result.status, LlmStatus::ToolCalls, "{:?}", result.error);
  let mut request = native_followup(&result);
  request
    .messages
    .push(json!({"role":"tool","tool_call_id":"call_second","content":"Second found."}));
  let (result, _) = generate(&service, "session", request, &server.config).await;
  assert_eq!(result.status, LlmStatus::Completed, "{:?}", result.error);
  let sent = server.requests();
  let messages = sent[1].body["messages"].as_array().unwrap();
  assert_eq!(messages.len(), 3);
  assert_eq!(messages[2]["role"], "user");
  assert_eq!(messages[2]["content"].as_array().unwrap().len(), 2);
  assert_eq!(messages[2]["content"][0]["tool_use_id"], "call_fixture");
  assert_eq!(messages[2]["content"][1]["tool_use_id"], "call_second");
  assert_eq!(messages[2]["content"][1]["content"], "Second found.");
}

#[tokio::test]
async fn chat_reserved_provider_state_never_produces_an_unreplayable_success() {
  for streaming in [false, true] {
    for state in [Value::Null, json!({"vendor":"opaque"})] {
      let message = json!({
        "role":"assistant", "content":"Visible partial.", "provider_state":state
      });
      let reply = if streaming {
        Reply::Stream(sse(json!({
          "model":"actual-model-revision",
          "choices":[{"index":0,"delta":message,"finish_reason":"stop"}]
        })))
      } else {
        Reply::Json(completion(message, "stop"))
      };
      let server = Server::start(vec![reply]).await;
      let service = LlmService::new().unwrap();
      service.register_session("session").unwrap();
      let mut request = input("reserved_state");
      request.stream = streaming;
      let (result, events) = generate(&service, "session", request, &server.config).await;
      assert_eq!(result.status, LlmStatus::Unsupported);
      assert_eq!(result.content, "Visible partial.");
      assert!(result.message.is_none());
      assert!(result.tool_calls.is_empty());
      assert_eq!(
        result.error.unwrap().capability.as_deref(),
        Some("reserved_provider_state")
      );
      assert_eq!(server.requests().len(), 1);
      assert_eq!(
        events
          .iter()
          .filter(|event| event.kind == "text_delta")
          .count(),
        usize::from(streaming)
      );
    }
  }
}

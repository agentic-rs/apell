use super::*;

fn request(id: &str) -> LlmRequest {
  LlmRequest {
    request_id: id.into(),
    messages: vec![LlmMessage {
      role: "user".into(),
      content: "Hello".into(),
      ..Default::default()
    }],
    ..Default::default()
  }
}

#[test]
fn validates_urls_schema_budget_and_tool_history() {
  for base_url in [
    "file:///tmp/test",
    "http://provider.invalid/v1",
    "https://user:secret@provider.invalid/v1",
    "https://provider.invalid/v1?key=secret",
    "https://provider.invalid/v1#fragment",
  ] {
    assert!(validate_config(&LlmConfig {
      base_url: base_url.into(),
      ..Default::default()
    })
    .is_err());
  }
  for base_url in [
    "https://provider.invalid/v1/",
    "http://127.0.0.1:8000/v1",
    "http://[::1]:8000/v1",
  ] {
    let config = LlmConfig {
      base_url: base_url.into(),
      ..Default::default()
    };
    assert_eq!(
      validation::completion_url(&config).unwrap().path(),
      "/v1/chat/completions"
    );
  }
  let mut invalid = request("id");
  invalid.tools = vec![LlmToolDefinition {
    name: "search".into(),
    description: None,
    parameters: json!({"description": "x".repeat(MAX_INPUT_BYTES)}),
  }];
  assert!(validation::request(&invalid).is_err());
  invalid.tools.clear();
  invalid.messages.push(LlmMessage {
    role: "tool".into(),
    content: "result".into(),
    tool_call_id: Some("unmatched".into()),
    ..Default::default()
  });
  assert!(validation::request(&invalid).is_err());
  invalid.messages.insert(
    1,
    LlmMessage {
      role: "assistant".into(),
      tool_calls: vec![LlmToolCall {
        id: "unmatched".into(),
        name: "search".into(),
        arguments: "{}".into(),
      }],
      ..Default::default()
    },
  );
  assert!(validation::request(&invalid).is_ok());
  invalid.temperature = Some(f64::NAN);
  assert!(validation::request(&invalid).is_err());
}

#[tokio::test]
async fn early_cancellation_and_close_are_scoped_and_release_registry() {
  let service = Arc::new(LlmService::new().unwrap());
  service.register_session("one").unwrap();
  service.register_session("two").unwrap();
  service.cancel("one", "early").unwrap();
  let result = service
    .generate("one", request("early"), LlmConfig::default(), None, |_| {})
    .await;
  assert_eq!(result.status, LlmStatus::Cancelled);
  assert!(service.register("two", "early").is_ok());
  let (mut receiver, guard) = service.register("one", "same").unwrap();
  service.cancel("two", "same").unwrap();
  assert!(!*receiver.borrow());
  service.cancel("one", "same").unwrap();
  canceled(&mut receiver).await;
  drop(guard);
  assert!(service.requests.lock().unwrap().active.is_empty());
  let spawned = service.clone();
  let task = tokio::spawn(async move {
    spawned
      .generate(
        "one",
        request("running"),
        LlmConfig::default(),
        None,
        |_| {},
      )
      .await
  });
  for _ in 0..100 {
    if !service.requests.lock().unwrap().active.is_empty() {
      break;
    }
    tokio::task::yield_now().await;
  }
  service.close_session("one");
  assert_eq!(task.await.unwrap().status, LlmStatus::Cancelled);
  assert!(service.requests.lock().unwrap().active.is_empty());
  assert!(service.register("one", "late").is_err());
}

#[test]
fn stale_guard_cannot_remove_replacement_request() {
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let (_, old) = service.register("session", "request").unwrap();
  service.close_session("session");
  service.register_session("session").unwrap();
  let (_, current) = service.register("session", "request").unwrap();
  drop(old);
  assert_eq!(service.requests.lock().unwrap().active.len(), 1);
  drop(current);
  assert!(service.requests.lock().unwrap().active.is_empty());
}

#[tokio::test]
async fn demo_terminal_matches_stream_and_legacy_contract() {
  let service = LlmService::new().unwrap();
  service.register_session("session").unwrap();
  let events = Arc::new(Mutex::new(Vec::new()));
  let sink = events.clone();
  let result = service
    .generate(
      "session",
      request("demo"),
      LlmConfig::default(),
      None,
      move |event| sink.lock().unwrap().push(event),
    )
    .await;
  assert_eq!(result.status, LlmStatus::Completed);
  assert_eq!(result.provenance.model_id.as_deref(), Some("local-demo"));
  {
    let events = events.lock().unwrap();
    assert_eq!(events.first().unwrap().kind, "started");
    assert_eq!(events.last().unwrap().kind, "terminal");
    assert_eq!(
      events
        .iter()
        .filter(|event| event.kind == "delta")
        .map(|event| event.text.as_str())
        .collect::<String>(),
      result.content
    );
    assert!(events.last().unwrap().result.is_some());
  }
  let mut tools = request("tools");
  tools.tools.push(LlmToolDefinition {
    name: "search".into(),
    description: None,
    parameters: json!({}),
  });
  assert_eq!(
    service
      .generate("session", tools.clone(), LlmConfig::default(), None, |_| {})
      .await
      .status,
    LlmStatus::Unsupported
  );
  assert!(service
    .complete("session", tools, LlmConfig::default(), None, |_| {})
    .await
    .is_err());
  service.cancel("session", "legacy").unwrap();
  assert!(service
    .complete(
      "session",
      request("legacy"),
      LlmConfig::default(),
      None,
      |event| assert_ne!(event.kind, "done")
    )
    .await
    .is_err());
}

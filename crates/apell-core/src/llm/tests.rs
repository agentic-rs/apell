use super::*;
use serde_json::json;

fn request(id: &str) -> LlmRequest {
  LlmRequest {
    request_id: id.into(),
    messages: vec![json!({"role":"user", "content":"Hello"})],
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
      provider_id: "custom".into(),
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
      provider_id: "custom".into(),
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
  invalid
    .messages
    .push(json!({"role":"tool", "content":"result", "tool_call_id":"unmatched"}));
  assert!(validation::request(&invalid).is_err());
  invalid.messages.insert(
    1,
    json!({"role":"assistant", "content":null, "tool_calls":[{
      "id":"unmatched", "type":"function", "function":{"name":"search", "arguments":"{}"}
    }]}),
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
async fn demo_result_matches_progress_and_returns_replayable_message() {
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
    assert_eq!(
      events
        .iter()
        .filter(|event| event.kind == "text_delta")
        .map(|event| event.text.as_str())
        .collect::<String>(),
      result.content
    );
    assert!(events
      .iter()
      .all(|event| matches!(event.kind.as_str(), "started" | "text_delta")));
    assert_eq!(
      result.message.as_ref().unwrap(),
      &json!({"role":"assistant", "content":result.content})
    );
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
  let mut buffered = request("buffered");
  buffered.stream = false;
  let result = service
    .generate("session", buffered, LlmConfig::default(), None, |event| {
      assert_eq!(event.kind, "started");
    })
    .await;
  assert_eq!(result.status, LlmStatus::Completed);
  assert!(result.message.is_some());
}

#[test]
fn validates_raw_message_shape_metadata_and_generation_options() {
  let mut input = request("raw");
  input.messages[0]["vendor_metadata"] = json!({"signature":[null, {"opaque":"data"}]});
  assert!(validation::request(&input).is_ok());
  for content in [json!([]), json!({}), json!(1), json!(true)] {
    input.messages[0]["content"] = content;
    assert!(validation::request(&input).is_err());
  }
  input.messages[0]["content"] = json!("hello");
  for limit in [0, 131_073, u32::MAX] {
    input.max_output_tokens = Some(limit);
    assert!(validation::request(&input).is_err());
  }
  for limit in [1, 131_072] {
    input.max_output_tokens = Some(limit);
    assert!(validation::request(&input).is_ok());
  }
  for limit in [json!(-1), json!(1.5), json!("10")] {
    assert!(serde_json::from_value::<LlmRequest>(json!({
      "request_id":"raw", "messages":input.messages, "max_output_tokens":limit
    }))
    .is_err());
  }
  let decoded: LlmRequest = serde_json::from_value(json!({
    "request_id":"raw", "messages":input.messages
  }))
  .unwrap();
  assert!(decoded.stream);
  assert!(LlmRequest::default().stream);
  let mut nested = json!(null);
  for _ in 0..61 {
    nested = json!([nested]);
  }
  input.messages[0]["vendor_metadata"] = nested.clone();
  assert!(validation::request(&input).is_ok());
  input.messages[0]["vendor_metadata"] = json!([nested]);
  assert!(validation::request(&input).is_err());
  input.messages[0]["vendor_metadata"] = json!("x".repeat(MAX_INPUT_BYTES));
  assert!(validation::request(&input).is_err());
}

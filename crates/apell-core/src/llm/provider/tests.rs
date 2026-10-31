use super::*;

fn request() -> LlmRequest {
  LlmRequest {
    request_id: "test".into(),
    tools: vec![super::super::LlmToolDefinition {
      name: "lookup".into(),
      description: None,
      parameters: json!({"type": "object"}),
    }],
    ..Default::default()
  }
}

fn output() -> Output {
  Output::new("test", LlmConfig::default().provenance())
}

#[test]
fn tools_accumulate_by_index_and_reasoning_remains_private() {
  let mut output = output();
  output
    .consume(
      &json!({"choices":[{"delta":{"reasoning_content":"Think "}}]}),
      true,
      &|_| {},
    )
    .unwrap();
  output.consume(&json!({"choices":[{"delta":{"reasoning_content":"more","tool_calls":[
    {"index":1,"id":"second","type":"function","function":{"name":"lookup","arguments":"{\"b\":"}},
    {"index":0,"id":"first","type":"function","function":{"name":"lookup","arguments":"{\"a\":"}}
  ]}}]}), true, &|_| {}).unwrap();
  output
    .consume(
      &json!({"choices":[{"delta":{"tool_calls":[
    {"index":0,"function":{"arguments":"1}"}}, {"index":1,"function":{"arguments":"2}"}}
  ]},"finish_reason":"tool_calls"}]}),
      true,
      &|_| {},
    )
    .unwrap();
  assert_eq!(output.finish(&request()).unwrap(), LlmStatus::ToolCalls);
  assert_eq!(output.tool_calls[0].arguments, "{\"a\":1}");
  assert_eq!(output.tool_calls[1].id, "second");
  assert_eq!(output.opaque["reasoning_content"], "Think more");
  let result = output.result(Ok(LlmStatus::ToolCalls));
  assert!(result.content.is_empty());
  assert!(!serde_json::to_string(&result)
    .unwrap()
    .contains("Think more"));
}

#[test]
fn empty_reasoning_fields_survive_continuation_without_ui_events() {
  use super::super::{continuation, LlmMessage};

  for streaming in [false, true] {
    let config = LlmConfig::default();
    let mut output = Output::new("test", config.provenance());
    output
      .fields(
        &json!({"reasoning_content":"", "reasoning":"", "reasoning_details":[]}),
        streaming,
        &|_| panic!("Empty reasoning must not emit UI events."),
      )
      .unwrap();
    let mut cache = continuation::ContinuationCache::default();
    let continuation_id = cache.insert("session", &config, &output).unwrap();
    assert!(continuation_id.is_some());
    let replay = continuation::messages(
      &mut cache,
      "session",
      &config,
      &[LlmMessage {
        role: "assistant".into(),
        continuation_id,
        ..Default::default()
      }],
    )
    .unwrap();
    assert_eq!(replay[0]["reasoning_content"], "");
    assert_eq!(replay[0]["reasoning"], "");
    assert_eq!(replay[0]["reasoning_details"], json!([]));
  }
}

#[test]
fn every_non_successful_finish_remains_distinct_and_partial() {
  for (reason, status, code) in [
    ("length", LlmStatus::Truncated, "truncated"),
    ("content_filter", LlmStatus::Failed, "content_filter"),
    ("other", LlmStatus::Failed, "unknown_finish_reason"),
  ] {
    let mut output = output();
    output
      .consume(
        &json!({"choices":[{"message":{"content":"partial"},"finish_reason":reason}]}),
        false,
        &|_| {},
      )
      .unwrap();
    let finish = output.finish(&request());
    let result = output.result(finish);
    assert_eq!(result.status, status);
    assert_eq!(result.error.unwrap().code, code);
    assert_eq!(result.content, "partial");
  }
  let mut output = output();
  output
    .consume(
      &json!({"choices":[{"delta":{"content":"partial"}}]}),
      true,
      &|_| {},
    )
    .unwrap();
  assert_eq!(
    output.finish(&request()).unwrap_err().code,
    "incomplete_response"
  );
}

#[test]
fn required_named_and_none_choices_cannot_silently_succeed() {
  for choice in [
    LlmToolChoice::Mode("required".into()),
    LlmToolChoice::Named {
      name: "lookup".into(),
    },
  ] {
    let mut request = request();
    request.tool_choice = Some(choice);
    let mut output = output();
    output
      .consume(
        &json!({"choices":[{"message":{"content":"ignored"},"finish_reason":"stop"}]}),
        false,
        &|_| {},
      )
      .unwrap();
    assert_eq!(
      output.finish(&request).unwrap_err().code,
      "tool_choice_not_honored"
    );
  }
  let mut request = request();
  request.tool_choice = Some(LlmToolChoice::Mode("none".into()));
  let mut output = output();
  output.consume(&json!({"choices":[{"message":{"tool_calls":[{"id":"call","type":"function","function":{"name":"lookup","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}), false, &|_| {}).unwrap();
  assert_eq!(
    output.finish(&request).unwrap_err().code,
    "tool_choice_not_honored"
  );
}

#[test]
fn rejects_inconsistent_metadata_oversize_and_unsupported_private_formats() {
  let mut output = Output::new(
    "test",
    LlmProvenance {
      model_id: None,
      ..LlmConfig::default().provenance()
    },
  );
  output
    .consume(
      &json!({"id":"one","model":"actual","choices":[]}),
      true,
      &|_| {},
    )
    .unwrap();
  assert!(output
    .consume(
      &json!({"id":"two","model":"actual","choices":[]}),
      true,
      &|_| {}
    )
    .is_err());
  assert!(output
    .consume(
      &json!({"id":"one","model":"different","choices":[]}),
      true,
      &|_| {}
    )
    .is_err());
  assert!(output
    .fields(
      &json!({"content":"x".repeat(MAX_OUTPUT_BYTES + 1)}),
      true,
      &|_| {}
    )
    .is_err());
  let mut output = Output::new("test", LlmConfig::default().provenance());
  assert_eq!(
    output
      .fields(
        &json!({"reasoning_details":[{"type":"reasoning.encrypted","data":"opaque"}]}),
        true,
        &|_| {}
      )
      .unwrap_err()
      .capability
      .as_deref(),
    Some("continuation_format")
  );
  assert!(output
    .fields(
      &json!({"reasoning_details":[{"type":"reasoning.encrypted","data":"opaque"}]}),
      false,
      &|_| {}
    )
    .is_ok());
  assert_eq!(output.opaque["reasoning_details"][0]["data"], "opaque");
}

#[test]
fn provider_errors_are_classified_without_echoing_private_body() {
  let generic = provider_error(
    &json!({"error":{"code":"invalid_request_error","message":"secret provider detail"}}),
    Some(400),
  );
  assert_eq!(generic.code, "provider_error");
  assert!(!generic.message.contains("secret"));
  let unsupported = provider_error(
    &json!({"error":{"code":"unsupported_parameter","param":"tools","message":"secret"}}),
    Some(400),
  );
  assert_eq!(unsupported.capability.as_deref(), Some("tools"));
}

#[test]
fn partial_calls_and_unknown_nested_metadata_are_never_executable() {
  let mut output = output();
  output.consume(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"lookup","arguments":"{\"partial\":"}}]},"finish_reason":"length"}]}), true, &|_| {}).unwrap();
  let finish = output.finish(&request());
  let result = output.result(finish);
  assert_eq!(result.status, LlmStatus::Truncated);
  assert!(result.tool_calls.is_empty());
  let mut output = Output::new("test", LlmConfig::default().provenance());
  assert_eq!(output.fields(&json!({"tool_calls":[{"id":"call","type":"function","function":{"name":"lookup","arguments":"{}"},"extra_content":{"google":{"thought_signature":"opaque"}}}]}), false, &|_| {}).unwrap_err().capability.as_deref(), Some("continuation_format"));
}

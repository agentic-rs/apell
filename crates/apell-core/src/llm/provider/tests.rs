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
fn tools_and_reasoning_reconstruct_a_complete_assistant_message() {
  let mut output = output();
  output
    .consume(
      &json!({"choices":[{"delta":{"role":"assistant", "reasoning_content":"Think "}}]}),
      true,
      &|_| {},
    )
    .unwrap();
  output.consume(&json!({"choices":[{"delta":{"reasoning_content":"more","tool_calls":[
    {"index":1,"id":"second","type":"function","function":{"name":"look","arguments":"{\"b\":"}},
    {"index":0,"id":"first","type":"function"}
  ]}}]}), true, &|_| {}).unwrap();
  output
    .consume(
      &json!({"choices":[{"delta":{"tool_calls":[
    {"index":0,"function":{"name":"lookup","arguments":"{\"a\":1}"}},
    {"index":1,"function":{"name":"up","arguments":"2}"}}
  ]},"finish_reason":"tool_calls"}]}),
      true,
      &|_| {},
    )
    .unwrap();
  let status = output.finish(&request()).unwrap();
  assert_eq!(status, LlmStatus::ToolCalls);
  let result = output.result(Ok(status));
  assert_eq!(result.tool_calls[0].arguments, "{\"a\":1}");
  assert_eq!(result.tool_calls[1].id, "second");
  assert!(result.content.is_empty());
  assert_eq!(
    result.message.unwrap(),
    json!({
      "role":"assistant", "content":null, "reasoning_content":"Think more",
      "tool_calls":[
        {"id":"first", "type":"function", "function":{"name":"lookup", "arguments":"{\"a\":1}"}},
        {"id":"second", "type":"function", "function":{"name":"lookup", "arguments":"{\"b\":2}"}}
      ]
    })
  );
}

#[test]
fn empty_and_null_metadata_are_preserved_without_spurious_events() {
  for streaming in [false, true] {
    let mut output = output();
    let fields = json!({"role":"assistant", "content":null, "reasoning_content":"", "reasoning":"", "reasoning_details":[], "annotations":[], "unknown":null});
    output
      .fields(&fields, streaming, &|_| {
        panic!("Empty fields must not emit UI events.")
      })
      .unwrap();
    assert_eq!(Value::Object(output.message), fields);
  }
}

#[test]
fn buffered_messages_preserve_arbitrary_metadata_at_every_level() {
  let message = json!({
    "role":"assistant", "content":null, "reasoning":{"data":[true, null, 3]},
    "reasoning_details":[{"type":"reasoning.encrypted", "data":"opaque"}],
    "annotations":[{"source":"fixture"}], "vendor_field":{"a":1},
    "tool_calls":[{"id":"call", "type":"function", "function":{
      "name":"lookup", "arguments":"{}", "nested":{"signature":"signed"}
    }, "extra_content":{"google":{"thought_signature":"opaque"}}}]
  });
  let mut output = output();
  output
    .consume(
      &json!({"choices":[{"message":message,"finish_reason":"tool_calls"}]}),
      false,
      &|_| {},
    )
    .unwrap();
  let status = output.finish(&request());
  let result = output.result(status);
  assert_eq!(result.status, LlmStatus::ToolCalls);
  assert_eq!(result.message, Some(message));
}

#[test]
fn every_non_successful_finish_keeps_partial_text_but_no_replay_message() {
  for (reason, status, code) in [
    ("length", LlmStatus::Truncated, "truncated"),
    ("content_filter", LlmStatus::Failed, "content_filter"),
    ("other", LlmStatus::Failed, "unknown_finish_reason"),
  ] {
    let mut output = output();
    output.consume(&json!({"choices":[{"message":{"role":"assistant", "content":"partial"},"finish_reason":reason}]}), false, &|_| {}).unwrap();
    let finish = output.finish(&request());
    let result = output.result(finish);
    assert_eq!(result.status, status);
    assert_eq!(result.error.unwrap().code, code);
    assert_eq!(result.content, "partial");
    assert!(result.message.is_none());
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
    output.consume(&json!({"choices":[{"message":{"role":"assistant", "content":"ignored"},"finish_reason":"stop"}]}), false, &|_| {}).unwrap();
    assert_eq!(
      output.finish(&request).unwrap_err().code,
      "tool_choice_not_honored"
    );
  }
  let mut request = request();
  request.tool_choice = Some(LlmToolChoice::Mode("none".into()));
  let mut output = output();
  output.consume(&json!({"choices":[{"message":{"role":"assistant", "tool_calls":[{"id":"call","type":"function","function":{"name":"lookup","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}), false, &|_| {}).unwrap();
  assert_eq!(
    output.finish(&request).unwrap_err().code,
    "tool_choice_not_honored"
  );
}

#[test]
fn rejects_inconsistent_metadata_and_oversized_output() {
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
  assert!(output
    .fields(
      &json!({"role":"assistant", "content":"text", "metadata":"x".repeat(MAX_OUTPUT_BYTES)}),
      false,
      &|_| {}
    )
    .is_err());
}

#[test]
fn unknown_stream_merge_formats_fail_explicitly_instead_of_losing_metadata() {
  for fields in [
    json!({"reasoning_details":[{"type":"reasoning.encrypted","data":"opaque"}]}),
    json!({"annotations":[{"source":"fixture"}]}),
    json!({"signature":"opaque"}),
    json!({"tool_calls":[{"index":0,"id":"call","type":"function","function":{"name":"lookup","arguments":"{}"},"extra_content":{"google":{"thought_signature":"opaque"}}}]}),
    json!({"tool_calls":[{"index":0,"function":{"name":"lookup","arguments":"{}","metadata":{"signature":"opaque"}}}]}),
  ] {
    let mut output = output();
    let error = output.fields(&fields, true, &|_| {}).unwrap_err();
    assert_eq!(error.capability.as_deref(), Some("stream_metadata"));
    assert!(error.message.contains("stream: false"));
    assert!(output.result(Err(error)).message.is_none());
  }
}

#[test]
fn successful_messages_must_pass_the_same_shape_checks_as_replayed_input() {
  let mut nested = json!({});
  for _ in 0..62 {
    nested = json!({"nested":nested});
  }
  for message in [
    json!({"role":"assistant", "content":"Answer", "tool_call_id":"invalid-for-assistant"}),
    json!({"role":"assistant", "content":"Answer", "metadata":nested}),
  ] {
    let mut output = output();
    output
      .consume(
        &json!({"choices":[{"message":message,"finish_reason":"stop"}]}),
        false,
        &|_| {},
      )
      .unwrap();
    let finish = output.finish(&request());
    let result = output.result(finish);
    assert_eq!(result.status, LlmStatus::Failed);
    assert_eq!(result.content, "Answer");
    assert_eq!(result.error.unwrap().code, "invalid_response");
    assert!(result.message.is_none());
  }
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
fn partial_calls_are_never_executable_or_replayable() {
  let mut output = output();
  output.consume(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"lookup","arguments":"{\"partial\":"}}]},"finish_reason":"length"}]}), true, &|_| {}).unwrap();
  let finish = output.finish(&request());
  let result = output.result(finish);
  assert_eq!(result.status, LlmStatus::Truncated);
  assert!(result.tool_calls.is_empty());
  assert!(result.message.is_none());
}

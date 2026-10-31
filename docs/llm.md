# Applet LLM tool continuation

Apell owns model access. The applet owns context, tools, validation, review, and storage. No provider key or provider-specific continuation field belongs in the applet.

Use the SDK's `llm.generate` method. It resolves with an explicit terminal outcome; only `completed` is a completed answer, while `tool_calls` hands control to the applet. The bridge can still reject if the host is unavailable or the applet session is closed. Handle both paths.

This example proposes a draft for review. `validateDraft` and `showDraftForReview` are application functions; they do not belong in Apell. A tool result acknowledges preparation of a draft, not a saved annotation. The user decides whether to save it.

```ts
// Import path when used from examples/chatbot/src; adjust for your applet.
import {
  createId,
  llm,
  onLlmEvent,
  type LlmMessage,
  type LlmTool,
} from '../../../packages/applet-sdk/src';

const tools: LlmTool[] = [
  {
    name: 'propose_annotation',
    description: 'Propose an annotation draft for user review.',
    parameters: {
      type: 'object',
      properties: { text: { type: 'string' } },
      required: ['text'],
      additionalProperties: false,
    },
  },
];
const messages: LlmMessage[] = [
  { role: 'system', content: 'Answer the question and propose a draft when useful.' },
  { role: 'user', content: selectedPassageAndQuestion },
];
const request_id = createId();
const unsubscribe = onLlmEvent((event) => {
  if (event.request_id !== request_id) return;
  if (event.kind === 'delta') showAnswerDelta(event.text);
  if (event.kind === 'reasoning_delta') showTransientReasoning(event.text);
});

try {
  const result = await llm.generate({ request_id, messages, tools, tool_choice: 'auto' });
  if (result.status === 'completed') {
    showCompletedAnswer(result.content, result.provenance);
  } else if (result.status === 'tool_calls') {
    messages.push({
      role: 'assistant',
      content: result.content,
      tool_calls: result.tool_calls,
      continuation_id: result.continuation_id,
    });
    for (const call of result.tool_calls) {
      let outcome: { accepted: boolean; reason?: string };
      try {
        if (call.name !== 'propose_annotation') throw new Error('Unknown tool');
        const draft = validateDraft(JSON.parse(call.arguments));
        showDraftForReview(draft, result.provenance);
        outcome = { accepted: true };
      } catch {
        outcome = { accepted: false, reason: 'The proposed draft did not pass validation.' };
      }
      messages.push({ role: 'tool', tool_call_id: call.id, content: JSON.stringify(outcome) });
    }
    const continuation = await llm.generate({
      request_id: createId(),
      messages,
      tools,
      tool_choice: 'none',
    });
    if (continuation.status === 'completed') {
      showCompletedAnswer(continuation.content, continuation.provenance);
    } else {
      showInterruptedAnswer(continuation.content, continuation.status, continuation.error);
    }
  } else {
    showInterruptedAnswer(result.content, result.status, result.error);
  }
} catch (error) {
  showHostFailure(error);
} finally {
  unsubscribe();
  clearTransientReasoning();
}
```

The example subscribes only to the first turn; subscribe with the continuation request ID as well if its answer should stream. Cancellation uses `await llm.cancel(request_id)` for the active turn.

Return every assistant continuation handle unchanged, including handles on older messages that remain in the context. Handles are transient: they expire, may be evicted, and are invalidated by applet closure or incompatible connection/model changes. On `invalid_continuation`, rebuild a fresh context or show a retry action; do not silently strip the handle and pretend the same provider turn resumed. Conversation trimming and retry policy stay in the applet.

`llm.config()` reports tri-state capabilities. `unknown` permits attempting a request; it does not guarantee success. Always inspect the outcome. To require a proposal, use `tool_choice: 'required'` or `{ name: 'propose_annotation' }`, then validate the result and apply any application-specific limit on the number of drafts. Schema-constrained answer generation is a separate future feature.

Persist model attribution from each result's `provenance`: `connection_id`, `requested_model_id`, and nullable actual `model_id`. Preserve a missing actual model as unknown. Keep reasoning and continuation handles out of durable notes. Never execute tools from partial stream fragments or save cancelled, truncated, unsupported, or failed responses as complete.

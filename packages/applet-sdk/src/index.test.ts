import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { LlmResult } from './types';

function llmResult(request_id: string, overrides: Partial<LlmResult> = {}): LlmResult {
  return {
    request_id,
    status: 'completed',
    content: 'An answer',
    tool_calls: [],
    message: { role: 'assistant', content: 'An answer' },
    finish_reason: 'stop',
    provenance: {
      connection_id: 'test_connection',
      requested_model_id: 'requested_model',
      model_id: 'reported_model',
    },
    error: null,
    ...overrides,
  };
}

describe('applet bridge', () => {
  let windowMock: EventTarget & { parent: { postMessage: ReturnType<typeof vi.fn> } };
  let channel: {
    onmessage: ((event: MessageEvent) => void) | null;
    postMessage: ReturnType<typeof vi.fn>;
    start: ReturnType<typeof vi.fn>;
  };

  function connect(source: object = windowMock.parent): void {
    const event = new Event('message');
    Object.defineProperties(event, {
      source: { value: source },
      data: { value: { type: 'apell:init' } },
      ports: { value: [channel] },
    });
    windowMock.dispatchEvent(event);
  }

  function receive(data: unknown): void {
    channel.onmessage?.({ data } as MessageEvent);
  }

  beforeEach(() => {
    vi.resetModules();
    windowMock = Object.assign(new EventTarget(), { parent: { postMessage: vi.fn() } });
    channel = { onmessage: null, postMessage: vi.fn(), start: vi.fn() };
    vi.stubGlobal('window', windowMock);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it('binds only the parent port and keeps identity out of requests', async () => {
    const sdk = await import('./index');
    expect(windowMock.parent.postMessage).toHaveBeenCalledWith({ type: 'apell:ready' }, '*');
    connect({});
    expect(channel.start).not.toHaveBeenCalled();
    connect();
    const response = sdk.hostCall('sqlite.open', { database_name: 'chat' });
    await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledOnce());
    const request = channel.postMessage.mock.calls[0][0];
    expect(request).toEqual({
      type: 'request',
      request_id: expect.any(String),
      method: 'sqlite.open',
      params: { database_name: 'chat' },
    });
    receive({
      type: 'response',
      request_id: request.request_id,
      result: { database_handle: 'opaque' },
    });
    await expect(response).resolves.toEqual({ database_handle: 'opaque' });
    connect();
    expect(channel.start).toHaveBeenCalledOnce();
  });

  it.each([
    'speech.transcribe',
    'speech.download_model',
    'audio.start',
    'audio.import',
    'audio.share',
  ])('keeps %s pending until the host completes it', async (method) => {
    vi.useFakeTimers();
    const sdk = await import('./index');
    connect();
    const response = sdk.hostCall(method);
    const completed = expect(response).resolves.toBeNull();
    await vi.advanceTimersByTimeAsync(360_000);
    expect(channel.postMessage).toHaveBeenCalledOnce();
    const request = channel.postMessage.mock.calls[0][0];
    receive({ type: 'response', request_id: request.request_id, result: null });
    await completed;
  });

  it('waits for a backend command that takes longer than the generic bridge timeout', async () => {
    vi.useFakeTimers();
    const sdk = await import('./index');
    connect();
    const result = sdk.applet.invoke('generate_report', { conversation_id: 'one' });
    const completed = expect(result).resolves.toEqual({ report_id: 'stored-once' });
    await vi.advanceTimersByTimeAsync(65_000);
    expect(channel.postMessage).toHaveBeenCalledOnce();
    const request = channel.postMessage.mock.calls[0][0];
    expect(request).toMatchObject({
      method: 'backend.invoke',
      params: { command: 'generate_report', params: { conversation_id: 'one' } },
    });
    receive({
      type: 'response',
      request_id: request.request_id,
      result: { report_id: 'stored-once' },
    });
    await completed;
  });

  it('delivers backend events independently of service streams and honors unsubscribe', async () => {
    const sdk = await import('./index');
    connect();
    const handler = vi.fn();
    sdk.applet.onEvent(() => {
      throw new Error('Another view observer failed.');
    });
    const unsubscribe = sdk.applet.onEvent(handler);
    const event = { name: 'reply', payload: { revision: 7, text: 'Hello' } };
    receive({ type: 'event', event });
    receive({ type: 'backend_event', event: { ...event, payload: { revision: NaN } } });
    expect(handler).not.toHaveBeenCalled();
    receive({ type: 'backend_event', event });
    expect(handler).toHaveBeenCalledExactlyOnceWith(event);
    unsubscribe();
    receive({ type: 'backend_event', event });
    expect(handler).toHaveBeenCalledOnce();
  });

  it('returns partial text on failed streams for durable interrupted replies', async () => {
    const sdk = await import('./index');
    connect();
    const events: string[] = [];
    const on_event = vi.fn((event) => events.push(event.text));
    const response = sdk.llm.generate(
      { request_id: 'reply', messages: [{ role: 'user', content: 'Hello' }] },
      { on_event },
    );
    const rejected = expect(response).rejects.toMatchObject({
      message: 'Stopped',
      partial_content: 'Hello world',
    });
    await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledOnce());
    const request = channel.postMessage.mock.calls[0][0];
    receive({ type: 'event', event: { request_id: 'reply', kind: 'text_delta', text: 'Hello ' } });
    receive({ type: 'event', event: { request_id: 'reply', kind: 'text_delta', text: 'world' } });
    receive({ type: 'response', request_id: request.request_id, error: 'Stopped' });
    await rejected;
    expect(events).toEqual(['Hello ', 'world']);
    receive({ type: 'event', event: { request_id: 'reply', kind: 'text_delta', text: 'Late' } });
    expect(on_event).toHaveBeenCalledTimes(2);
  });

  it('waits for the picker and forwards only routing IDs from a public selection', async () => {
    vi.useFakeTimers();
    const sdk = await import('./index');
    connect();
    const target = { connection_id: 'saved', model_id: 'selected-model' };
    const selection = { ...target, provider_id: 'custom', provider_name: 'OpenAI compatible' };
    const choosing = sdk.llm.chooseModel({ current: selection });
    await vi.advanceTimersByTimeAsync(60_000);
    const picker = channel.postMessage.mock.calls[0][0];
    expect(picker.method).toBe('llm.choose_model');
    expect(picker.params).toEqual({ current: target });
    receive({ type: 'response', request_id: picker.request_id, result: selection });
    await expect(choosing).resolves.toEqual(selection);

    const config = sdk.llm.config(selection);
    const generation = sdk.llm.generate({
      messages: [{ role: 'user', content: 'Hello' }],
      target: selection,
    });
    await vi.advanceTimersByTimeAsync(0);
    const [, configCall, generateCall] = channel.postMessage.mock.calls.map(([call]) => call);
    expect(configCall.params).toEqual({ target });
    expect(generateCall.params.target).toEqual(target);
    receive({
      type: 'response',
      request_id: configCall.request_id,
      result: { ...selection, mode: 'provider' },
    });
    receive({
      type: 'response',
      request_id: generateCall.request_id,
      result: llmResult(generateCall.params.request_id),
    });
    await Promise.all([config, generation]);

    const cancelled = sdk.llm.chooseModel();
    await vi.advanceTimersByTimeAsync(0);
    const cancelCall = channel.postMessage.mock.calls.at(-1)![0];
    expect(cancelCall.params).toEqual({});
    receive({ type: 'response', request_id: cancelCall.request_id, result: null });
    await expect(cancelled).resolves.toBeNull();
  });

  it('delivers resource progress without treating it as an LLM event', async () => {
    const sdk = await import('./index');
    connect();
    const timer = vi.spyOn(globalThis, 'setTimeout');
    const onProgress = vi.fn();
    const unsubscribe = sdk.onDownloadProgress(onProgress);
    const response = sdk.hostCall('resources.download', {
      request_id: 'download',
      url: 'https://example.com/resource',
    });
    await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledOnce());
    expect(timer).toHaveBeenCalledWith(expect.any(Function), 135_000);
    const request = channel.postMessage.mock.calls[0][0];
    const progress = {
      request_id: 'download',
      kind: 'download_progress',
      received_bytes: 1024,
      total_bytes: null,
    };
    receive({ type: 'event', event: progress });
    receive({ type: 'event', event: { ...progress, received_bytes: -1 } });
    expect(onProgress).toHaveBeenCalledOnce();
    expect(onProgress).toHaveBeenCalledWith(progress);
    unsubscribe();
    receive({ type: 'event', event: { ...progress, received_bytes: 2048, total_bytes: 2048 } });
    expect(onProgress).toHaveBeenCalledOnce();
    receive({
      type: 'response',
      request_id: request.request_id,
      result: { resource_handle: 'resource' },
    });
    await expect(response).resolves.toEqual({ resource_handle: 'resource' });
  });

  it('rejects invalid configuration before transport and still permits explicit null', async () => {
    const sdk = await import('./index');
    connect();
    for (const value of [{ nested: [NaN] }, { nested: { limit: Infinity } }]) {
      await expect(sdk.config.set('saved', value)).rejects.toThrow(/finite numbers/);
    }
    expect(channel.postMessage).not.toHaveBeenCalled();
    const response = sdk.config.set('saved', null);
    await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledOnce());
    const request = channel.postMessage.mock.calls[0][0];
    expect(request.params).toEqual({ key: 'saved', value: null });
    receive({ type: 'response', request_id: request.request_id, result: null });
    await expect(response).resolves.toBeNull();
  });

  it('round-trips binary files, HTTP responses, and missing versus null config values', async () => {
    const sdk = await import('./index');
    connect();
    const timer = vi.spyOn(globalThis, 'setTimeout');
    const operations = [
      {
        call: () => sdk.fs.write('cache/data.bin', new Uint8Array([0, 128, 255])),
        method: 'fs.write',
        params: { path: 'cache/data.bin', bytes: [0, 128, 255] },
        result: { size_bytes: 3 },
      },
      {
        call: () => sdk.fs.read('cache/data.bin', { offset: 1, length: 2 }),
        method: 'fs.read',
        params: { path: 'cache/data.bin', offset: 1, length: 2 },
        result: { bytes: [128, 255], eof: true },
      },
      {
        call: () => sdk.http.request({ url: 'https://example.invalid/data', timeout_ms: 120_000 }),
        method: 'http.request',
        params: { url: 'https://example.invalid/data', timeout_ms: 120_000 },
        result: { status: 404, headers: [{ name: 'x-bytes', value: '\u00ff' }], body: [0, 255] },
      },
      {
        call: () => sdk.config.get('missing'),
        method: 'config.get',
        params: { key: 'missing' },
        result: { found: false },
      },
      {
        call: () => sdk.config.get('stored_null'),
        method: 'config.get',
        params: { key: 'stored_null' },
        result: { found: true, value: null },
      },
    ];
    for (const operation of operations) {
      channel.postMessage.mockClear();
      timer.mockClear();
      const response = operation.call();
      await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledOnce());
      const request = channel.postMessage.mock.calls[0][0];
      expect(request).toEqual({
        type: 'request',
        request_id: expect.any(String),
        method: operation.method,
        params: operation.params,
      });
      if (operation.method === 'http.request') {
        expect(timer).toHaveBeenCalledWith(expect.any(Function), 135_000);
      }
      receive({ type: 'response', request_id: request.request_id, result: operation.result });
      await expect(response).resolves.toEqual(operation.result);
    }
  });

  it('round-trips raw assistant metadata only when the applet submits a follow-up', async () => {
    const sdk = await import('./index');
    connect();
    const events: string[] = [];
    const tool = {
      name: 'lookup',
      description: undefined,
      parameters: {
        type: 'object',
        properties: { query: { type: 'string' } },
        required: ['query'],
      },
    };
    const response = sdk.llm.generate(
      {
        request_id: 'tool_round',
        messages: [{ role: 'user', content: 'Find an answer' }],
        tools: [tool],
        tool_choice: 'auto',
        temperature: undefined,
        stream: true,
        max_output_tokens: 1024,
      },
      { on_event: (event) => events.push(event.kind) },
    );
    await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledOnce());
    const request = channel.postMessage.mock.calls[0][0];
    expect(request.method).toBe('llm.generate');
    expect(request.params).not.toHaveProperty('temperature');
    expect(request.params.tools[0]).not.toHaveProperty('description');
    expect(request.params.stream).toBe(true);
    expect(request.params.max_output_tokens).toBe(1024);
    const tool_call = { id: 'call_one', name: 'lookup', arguments: '{"query":"example"}' };
    const outcome = llmResult('tool_round', {
      status: 'tool_calls',
      content: '',
      tool_calls: [tool_call],
      message: {
        role: 'assistant',
        content: null,
        tool_calls: [
          {
            id: 'call_one',
            type: 'function',
            function: { name: 'lookup', arguments: '{"query":"example"}', provider_extension: {} },
            extra_content: { signature: 'provider-signature' },
          },
        ],
        reasoning_content: '',
        reasoning_details: [{ type: 'reasoning.encrypted', data: 'provider-state' }],
        provider_extension: { nested: [true, null, 'preserved'] },
      },
      finish_reason: 'tool_calls',
    });
    receive({
      type: 'event',
      event: {
        request_id: 'tool_round',
        kind: 'started',
        text: '',
        provenance: outcome.provenance,
      },
    });
    receive({
      type: 'event',
      event: { request_id: 'tool_round', kind: 'reasoning_delta', text: 'Provider reasoning' },
    });
    receive({ type: 'response', request_id: request.request_id, result: outcome });
    await expect(response).resolves.toEqual(outcome);
    expect(events).toEqual(['started', 'reasoning_delta']);
    expect(channel.postMessage).toHaveBeenCalledOnce();

    const messages = [
      { role: 'user' as const, content: 'Find an answer' },
      outcome.message!,
      { role: 'tool' as const, content: 'The applet found a result', tool_call_id: tool_call.id },
    ];
    const follow_up = sdk.llm.generate({
      request_id: 'final_round',
      messages,
      tool_choice: 'none',
      stream: false,
    });
    await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledTimes(2));
    const next = channel.postMessage.mock.calls[1][0];
    expect(next.params.messages).toEqual(messages);
    receive({ type: 'response', request_id: next.request_id, result: llmResult('final_round') });
    await expect(follow_up).resolves.toEqual(llmResult('final_round'));
  });

  it('resolves terminal failures with authoritative partial content', async () => {
    const sdk = await import('./index');
    connect();
    for (const status of ['cancelled', 'truncated', 'unsupported', 'failed'] as const) {
      channel.postMessage.mockClear();
      const response = sdk.llm.generate({
        request_id: status,
        messages: [{ role: 'user', content: 'Question' }],
      });
      await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledOnce());
      const request = channel.postMessage.mock.calls[0][0];
      receive({
        type: 'event',
        event: { request_id: status, kind: 'text_delta', text: 'Only the received prefix' },
      });
      const outcome = llmResult(status, {
        status,
        content: 'Authoritative partial answer',
        message: null,
        error: { code: status, message: 'The reply did not complete.', retryable: false },
      });
      receive({ type: 'response', request_id: request.request_id, result: outcome });
      await expect(response).resolves.toEqual(outcome);
    }
  });

  it('keeps the original partial answer when a duplicate request is rejected and excludes reasoning', async () => {
    const sdk = await import('./index');
    connect();
    const response = sdk.llm.generate({
      request_id: 'shared',
      messages: [{ role: 'user', content: 'Question' }],
    });
    const rejected = expect(response).rejects.toMatchObject({
      message: 'Bridge failed',
      partial_content: 'Answer prefix',
    });
    await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledOnce());
    const request = channel.postMessage.mock.calls[0][0];
    receive({
      type: 'event',
      event: { request_id: 'shared', kind: 'reasoning_delta', text: 'Transient reasoning' },
    });
    receive({
      type: 'event',
      event: { request_id: 'shared', kind: 'text_delta', text: 'Answer prefix' },
    });
    await expect(
      sdk.hostCall('llm.generate', { request_id: 'shared', messages: [] }),
    ).rejects.toThrow('already active');
    expect(channel.postMessage).toHaveBeenCalledOnce();
    receive({ type: 'response', request_id: request.request_id, error: 'Bridge failed' });
    await rejected;
  });

  it('rejects invalid tool parameters before transport and releases IDs after transport errors', async () => {
    const sdk = await import('./index');
    connect();
    await expect(
      sdk.llm.generate({
        request_id: 'invalid',
        messages: [{ role: 'user', content: 'Question' }],
        tools: [{ name: 'lookup', parameters: { limit: Infinity } }],
      }),
    ).rejects.toThrow(/finite numbers/);
    expect(channel.postMessage).not.toHaveBeenCalled();
    channel.postMessage.mockImplementationOnce(() => {
      throw new Error('Clone failed');
    });
    const request = {
      request_id: 'retry',
      messages: [{ role: 'user' as const, content: 'Question' }],
    };
    await expect(sdk.llm.generate(request)).rejects.toMatchObject({
      name: 'HostError',
      message: 'Clone failed',
      partial_content: '',
    });
    const response = sdk.llm.generate(request);
    await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledTimes(2));
    const sent = channel.postMessage.mock.calls[1][0];
    receive({ type: 'response', request_id: sent.request_id, result: llmResult('retry') });
    await expect(response).resolves.toEqual(llmResult('retry'));
  });

  it('keeps concurrent answers separate and cancels only the timed-out request', async () => {
    const sdk = await import('./index');
    connect();
    vi.useFakeTimers();
    try {
      const slow = sdk.llm.generate({
        request_id: 'slow',
        messages: [{ role: 'user', content: 'Slow question' }],
      });
      const rejected = expect(slow).rejects.toMatchObject({
        name: 'HostError',
        message: 'The host request timed out.',
        partial_content: 'Slow answer',
      });
      const fast = sdk.llm.generate({
        request_id: 'fast',
        messages: [{ role: 'user', content: 'Fast question' }],
      });
      await vi.advanceTimersByTimeAsync(0);
      expect(channel.postMessage).toHaveBeenCalledTimes(2);
      for (const [request_id, kind, text] of [
        ['slow', 'reasoning_delta', 'Private reasoning'],
        ['slow', 'text_delta', 'Slow answer'],
        ['fast', 'text_delta', 'Fast answer'],
      ]) {
        receive({ type: 'event', event: { request_id, kind, text } });
      }
      const fastRequest = channel.postMessage.mock.calls[1][0];
      receive({
        type: 'response',
        request_id: fastRequest.request_id,
        result: llmResult('fast'),
      });
      await expect(fast).resolves.toEqual(llmResult('fast'));
      await vi.advanceTimersByTimeAsync(330_000);
      await rejected;
      expect(channel.postMessage).toHaveBeenCalledTimes(3);
      const cancel = channel.postMessage.mock.calls[2][0];
      expect(cancel).toMatchObject({ method: 'llm.cancel', params: { request_id: 'slow' } });
      receive({ type: 'response', request_id: cancel.request_id, result: null });
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it('generates IDs and isolates callbacks, including observer errors and late events', async () => {
    const sdk = await import('./index');
    connect();
    const firstEvents = vi.fn(() => {
      throw new Error('UI observer failed');
    });
    const secondEvents = vi.fn();
    const controller = new AbortController();
    const first = sdk.llm.generate(
      { messages: [{ role: 'user', content: 'First' }] },
      { on_event: firstEvents, signal: controller.signal },
    );
    const second = sdk.llm.generate(
      { request_id: 'second', messages: [{ role: 'user', content: 'Second' }] },
      { on_event: secondEvents },
    );
    await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledTimes(2));
    const [firstRequest, secondRequest] = channel.postMessage.mock.calls.map(([value]) => value);
    const first_id = firstRequest.params.request_id;
    expect(first_id).toEqual(expect.any(String));
    expect(first_id).not.toBe(firstRequest.request_id);
    expect(firstRequest.params).not.toHaveProperty('on_event');
    expect(firstRequest.params).not.toHaveProperty('signal');
    expect(() =>
      receive({
        type: 'event',
        event: { request_id: first_id, kind: 'text_delta', text: 'First' },
      }),
    ).not.toThrow();
    receive({
      type: 'event',
      event: { request_id: 'second', kind: 'reasoning_delta', text: 'Second thought' },
    });
    for (const kind of ['delta', 'terminal', 'tool_call', 'done', 'error']) {
      receive({ type: 'event', event: { request_id: first_id, kind, text: 'Obsolete' } });
    }
    receive({
      type: 'event',
      event: { request_id: 'unknown', kind: 'text_delta', text: 'Ignored' },
    });
    expect(firstEvents).toHaveBeenCalledOnce();
    expect(secondEvents).toHaveBeenCalledOnce();
    receive({ type: 'response', request_id: firstRequest.request_id, result: llmResult(first_id) });
    await expect(first).resolves.toEqual(llmResult(first_id));
    controller.abort();
    receive({ type: 'event', event: { request_id: first_id, kind: 'text_delta', text: 'Late' } });
    expect(firstEvents).toHaveBeenCalledOnce();
    expect(channel.postMessage).toHaveBeenCalledTimes(2);
    receive({
      type: 'response',
      request_id: secondRequest.request_id,
      result: llmResult('second'),
    });
    await expect(second).resolves.toEqual(llmResult('second'));
  });

  it('sends early cancellation before generation and awaits the native cancelled result', async () => {
    const sdk = await import('./index');
    vi.useFakeTimers();
    const controller = new AbortController();
    const removed = vi.spyOn(controller.signal, 'removeEventListener');
    const on_event = vi.fn();
    const response = sdk.llm.generate(
      { request_id: 'early', messages: [{ role: 'user', content: 'Question' }] },
      { signal: controller.signal, on_event },
    );
    controller.abort();
    expect(channel.postMessage).not.toHaveBeenCalled();
    connect();
    await vi.advanceTimersByTimeAsync(0);
    expect(channel.postMessage).toHaveBeenCalledTimes(2);
    const [cancel, generate] = channel.postMessage.mock.calls.map(([value]) => value);
    expect(cancel).toMatchObject({ method: 'llm.cancel', params: { request_id: 'early' } });
    expect(generate.method).toBe('llm.generate');
    const outcome = llmResult('early', {
      status: 'cancelled',
      content: '',
      message: null,
      error: { code: 'cancelled', message: 'Cancelled.', retryable: false },
    });
    receive({ type: 'response', request_id: generate.request_id, result: outcome });
    await expect(response).resolves.toEqual(outcome);
    receive({ type: 'event', event: { request_id: 'early', kind: 'started', text: '' } });
    expect(on_event).not.toHaveBeenCalled();
    expect(removed).toHaveBeenCalledWith('abort', expect.any(Function));
    expect(vi.getTimerCount()).toBe(0);
  });

  it('cancels an active request once without replacing native partial content', async () => {
    const sdk = await import('./index');
    connect();
    vi.useFakeTimers();
    const controller = new AbortController();
    const on_event = vi.fn();
    const response = sdk.llm.generate(
      { request_id: 'active', messages: [{ role: 'user', content: 'Question' }] },
      { signal: controller.signal, on_event },
    );
    let settled = false;
    void response.then(() => {
      settled = true;
    });
    await vi.advanceTimersByTimeAsync(0);
    const generate = channel.postMessage.mock.calls[0][0];
    receive({
      type: 'event',
      event: { request_id: 'active', kind: 'text_delta', text: 'Visible prefix' },
    });
    controller.abort();
    controller.abort();
    await vi.advanceTimersByTimeAsync(0);
    expect(settled).toBe(false);
    expect(channel.postMessage).toHaveBeenCalledTimes(2);
    expect(channel.postMessage.mock.calls[1][0]).toMatchObject({
      method: 'llm.cancel',
      params: { request_id: 'active' },
    });
    const outcome = llmResult('active', {
      status: 'cancelled',
      content: 'Authoritative partial answer',
      message: null,
      error: { code: 'cancelled', message: 'Cancelled.', retryable: false },
    });
    receive({ type: 'response', request_id: generate.request_id, result: outcome });
    await expect(response).resolves.toEqual(outcome);
    receive({
      type: 'event',
      event: { request_id: 'active', kind: 'reasoning_delta', text: 'Late thought' },
    });
    expect(on_event).toHaveBeenCalledOnce();
    expect(vi.getTimerCount()).toBe(0);
  });

  it('cleans aborted requests when host connection fails and permits a fresh attempt', async () => {
    const sdk = await import('./index');
    vi.useFakeTimers();
    const controller = new AbortController();
    controller.abort();
    const removed = vi.spyOn(controller.signal, 'removeEventListener');
    const request = {
      request_id: 'connect_retry',
      messages: [{ role: 'user' as const, content: 'Question' }],
    };
    const response = sdk.llm.generate(request, { signal: controller.signal });
    const rejected = expect(response).rejects.toMatchObject({
      name: 'HostError',
      partial_content: '',
      message: expect.stringContaining('did not connect'),
    });
    await vi.advanceTimersByTimeAsync(15_000);
    await rejected;
    expect(removed).toHaveBeenCalledWith('abort', expect.any(Function));
    expect(vi.getTimerCount()).toBe(0);
    connect();
    const retry = sdk.llm.generate(request);
    await vi.advanceTimersByTimeAsync(0);
    expect(channel.postMessage).toHaveBeenCalledOnce();
    const sent = channel.postMessage.mock.calls[0][0];
    expect(sent.method).toBe('llm.generate');
    receive({ type: 'response', request_id: sent.request_id, result: llmResult('connect_retry') });
    await expect(retry).resolves.toEqual(llmResult('connect_retry'));
    expect(vi.getTimerCount()).toBe(0);
  });
});

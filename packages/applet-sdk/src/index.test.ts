import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { LlmResult } from './types';

function llmResult(request_id: string, overrides: Partial<LlmResult> = {}): LlmResult {
  return {
    request_id,
    status: 'completed',
    content: 'An answer',
    tool_calls: [],
    continuation_id: null,
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

  it('returns partial text on failed streams for durable interrupted replies', async () => {
    const sdk = await import('./index');
    connect();
    const events: string[] = [];
    const unsubscribe = sdk.onLlmEvent((event) => {
      events.push(event.text);
    });
    const response = sdk.hostCall('llm.complete', { request_id: 'reply', messages: [] });
    const rejected = expect(response).rejects.toMatchObject({
      message: 'Stopped',
      partial_content: 'Hello world',
    });
    await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledOnce());
    const request = channel.postMessage.mock.calls[0][0];
    receive({ type: 'event', event: { request_id: 'reply', kind: 'delta', text: 'Hello ' } });
    receive({ type: 'event', event: { request_id: 'reply', kind: 'delta', text: 'world' } });
    receive({ type: 'response', request_id: request.request_id, error: 'Stopped' });
    await rejected;
    expect(events).toEqual(['Hello ', 'world']);
    unsubscribe();
  });

  it('delivers resource progress without treating it as an LLM event', async () => {
    const sdk = await import('./index');
    connect();
    const timer = vi.spyOn(globalThis, 'setTimeout');
    const onProgress = vi.fn();
    const onLlm = vi.fn();
    const unsubscribe = sdk.onDownloadProgress(onProgress);
    sdk.onLlmEvent(onLlm);
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
    expect(onLlm).not.toHaveBeenCalled();
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

  it('round-trips tool calls and opaque continuation only when the applet submits a follow-up', async () => {
    const sdk = await import('./index');
    connect();
    const events: string[] = [];
    sdk.onLlmEvent((event) => events.push(event.kind));
    const tool = {
      name: 'lookup',
      description: undefined,
      parameters: {
        type: 'object',
        properties: { query: { type: 'string' } },
        required: ['query'],
      },
    };
    const response = sdk.llm.generate({
      request_id: 'tool_round',
      messages: [{ role: 'user', content: 'Find an answer' }],
      tools: [tool],
      tool_choice: 'auto',
      temperature: undefined,
    });
    await vi.waitFor(() => expect(channel.postMessage).toHaveBeenCalledOnce());
    const request = channel.postMessage.mock.calls[0][0];
    expect(request.method).toBe('llm.generate');
    expect(request.params).not.toHaveProperty('temperature');
    expect(request.params.tools[0]).not.toHaveProperty('description');
    const tool_call = { id: 'call_one', name: 'lookup', arguments: '{"query":"example"}' };
    const outcome = llmResult('tool_round', {
      status: 'tool_calls',
      content: '',
      tool_calls: [tool_call],
      continuation_id: 'opaque_continuation',
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
    receive({
      type: 'event',
      event: { request_id: 'tool_round', kind: 'tool_call', text: '', tool_call },
    });
    receive({
      type: 'event',
      event: { request_id: 'tool_round', kind: 'terminal', text: '', result: outcome },
    });
    receive({ type: 'response', request_id: request.request_id, result: outcome });
    await expect(response).resolves.toEqual(outcome);
    expect(events).toEqual(['started', 'reasoning_delta', 'tool_call', 'terminal']);
    expect(channel.postMessage).toHaveBeenCalledOnce();

    const messages = [
      { role: 'user' as const, content: 'Find an answer' },
      {
        role: 'assistant' as const,
        content: outcome.content,
        tool_calls: outcome.tool_calls,
        continuation_id: outcome.continuation_id,
      },
      { role: 'tool' as const, content: 'The applet found a result', tool_call_id: tool_call.id },
    ];
    const follow_up = sdk.llm.generate({
      request_id: 'final_round',
      messages,
      tool_choice: 'none',
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
        event: { request_id: status, kind: 'delta', text: 'Only the received prefix' },
      });
      const outcome = llmResult(status, {
        status,
        content: 'Authoritative partial answer',
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
      event: { request_id: 'shared', kind: 'delta', text: 'Answer prefix' },
    });
    await expect(
      sdk.hostCall('llm.complete', { request_id: 'shared', messages: [] }),
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
        ['slow', 'delta', 'Slow answer'],
        ['fast', 'delta', 'Fast answer'],
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
    } finally {
      vi.useRealTimers();
    }
  });
});

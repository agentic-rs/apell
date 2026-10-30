import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

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
});

import { describe, expect, it, vi } from 'vitest';
import { attachBridge, isBridgeRequest } from './bridge';

describe('applet bridge boundary', () => {
  it('accepts scoped SDK operations and rejects administrative operations', () => {
    const request = { type: 'request', request_id: 'one', method: 'sqlite.query', params: {} };
    for (const method of [
      'sqlite.query',
      'llm.config',
      'llm.generate',
      'llm.complete',
      'llm.cancel',
      'http.request',
      'fs.read',
      'fs.write',
      'fs.list',
      'fs.stat',
      'fs.mkdir',
      'fs.remove',
      'fs.rename',
      'config.get',
      'config.set',
      'config.delete',
    ]) {
      expect(isBridgeRequest({ ...request, method })).toBe(true);
    }
    for (const method of ['llm.save_settings', 'applets.install', 'shell_command', '__proto__']) {
      expect(isBridgeRequest({ ...request, method })).toBe(false);
    }
    expect(isBridgeRequest({ ...request, params: null })).toBe(false);
    expect(isBridgeRequest({ ...request, request_id: '' })).toBe(false);
  });

  it('rejects tool schemas that native JSON serialization would silently alter', async () => {
    const nativeCall = vi.fn(async () => ({}));
    const port = {
      onmessage: null,
      postMessage: vi.fn(),
      start: vi.fn(),
      close: vi.fn(),
    } as unknown as MessagePort;
    const dispose = attachBridge(port, nativeCall);
    const params = {
      request_id: 'generation',
      messages: [{ role: 'user', content: 'Make a draft' }],
      tools: [{ name: 'draft', parameters: { type: 'object', maximum: NaN } }],
    };
    await port.onmessage?.({
      data: { type: 'request', request_id: 'schema', method: 'llm.generate', params },
    } as MessageEvent);
    expect(nativeCall).not.toHaveBeenCalled();
    expect(port.postMessage).toHaveBeenLastCalledWith({
      type: 'response',
      request_id: 'schema',
      error: expect.stringContaining('plain JSON'),
    });
    params.tools[0].parameters.maximum = 100;
    await port.onmessage?.({
      data: { type: 'request', request_id: 'valid', method: 'llm.generate', params },
    } as MessageEvent);
    expect(nativeCall).toHaveBeenCalledWith('llm.generate', params);
    dispose();
  });

  it('protects saved config from raw requests with nested nonfinite numbers', async () => {
    let saved: unknown = { theme: 'dark' };
    const nativeCall = vi.fn(async (_method: string, params: unknown) => {
      saved = JSON.parse(JSON.stringify((params as Record<string, unknown>).value));
      return null;
    });
    const port = {
      onmessage: null,
      postMessage: vi.fn(),
      start: vi.fn(),
      close: vi.fn(),
    } as unknown as MessagePort;
    const dispose = attachBridge(port, nativeCall);
    for (const value of [
      { nested: [NaN] },
      { nested: { limit: Infinity } },
      { nested: undefined },
    ]) {
      await port.onmessage?.({
        data: {
          type: 'request',
          request_id: 'raw',
          method: 'config.set',
          params: { key: 'saved', value },
        },
      } as MessageEvent);
      expect(port.postMessage).toHaveBeenLastCalledWith({
        type: 'response',
        request_id: 'raw',
        error: expect.stringContaining('plain JSON'),
      });
    }
    expect(nativeCall).not.toHaveBeenCalled();
    expect(saved).toEqual({ theme: 'dark' });
    await port.onmessage?.({
      data: {
        type: 'request',
        request_id: 'null',
        method: 'config.set',
        params: { key: 'saved', value: null },
      },
    } as MessageEvent);
    expect(nativeCall).toHaveBeenCalledOnce();
    expect(saved).toBeNull();
    dispose();
  });

  it('does not apply config value restrictions to optional request fields', async () => {
    const nativeCall = vi.fn(async () => ({ status: 200 }));
    const port = {
      onmessage: null,
      postMessage: vi.fn(),
      start: vi.fn(),
      close: vi.fn(),
    } as unknown as MessagePort;
    const dispose = attachBridge(port, nativeCall);
    const params = { url: 'https://example.invalid/', body: undefined };
    await port.onmessage?.({
      data: { type: 'request', request_id: 'http', method: 'http.request', params },
    } as MessageEvent);
    expect(nativeCall).toHaveBeenCalledWith('http.request', params);
    dispose();
  });
});

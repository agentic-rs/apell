import { describe, expect, it } from 'vitest';
import { isBridgeRequest } from './bridge';

describe('applet bridge boundary', () => {
  it('accepts scoped SDK operations and rejects administrative operations', () => {
    const request = { type: 'request', request_id: 'one', method: 'sqlite.query', params: {} };
    expect(isBridgeRequest(request)).toBe(true);
    for (const method of ['llm.save_settings', 'applets.install', 'shell_command', '__proto__']) {
      expect(isBridgeRequest({ ...request, method })).toBe(false);
    }
    expect(isBridgeRequest({ ...request, params: null })).toBe(false);
    expect(isBridgeRequest({ ...request, request_id: '' })).toBe(false);
  });
});

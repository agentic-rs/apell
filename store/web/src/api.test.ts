import { afterEach, describe, expect, it, vi } from 'vitest';
import { api, ApiError, errorMessage } from './api';

afterEach(() => vi.unstubAllGlobals());

describe('Store account requests', () => {
  it('distinguishes stale session requests from removed package access', () => {
    expect(errorMessage(403, { error: { code: 'invalid_session_request' } })).toBe(
      'Your sign-in state changed. Refresh this page and try again.',
    );
    expect(errorMessage(403, { error: { code: 'forbidden' } })).toContain(
      'no longer have permission',
    );
  });
  it('requires CSRF for mutations and keeps requests on the Store origin', async () => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ ok: true }));
    vi.stubGlobal('fetch', fetch);
    await expect(
      api('/api/v1/account/packages', { method: 'POST', body: { applet_id: 'example' } }),
    ).rejects.toMatchObject({ status: 401 });
    await expect(
      api('https://other.invalid/api/v1/account/packages', {
        method: 'POST',
        csrf_token: 'test-csrf',
      }),
    ).rejects.toThrow('Invalid Store endpoint');
    expect(fetch).not.toHaveBeenCalled();
    await api('/api/v1/account/packages', {
      method: 'POST',
      csrf_token: 'test-csrf',
      body: { applet_id: 'example' },
    });
    expect(fetch).toHaveBeenCalledWith(
      '/api/v1/account/packages',
      expect.objectContaining({
        method: 'POST',
        credentials: 'same-origin',
        redirect: 'error',
        headers: {
          Accept: 'application/json',
          'Content-Type': 'application/json',
          'x-csrf-token': 'test-csrf',
        },
        body: '{"applet_id":"example"}',
      }),
    );
  });

  it('lets the browser generate multipart boundaries while retaining CSRF protection', async () => {
    const fetch = vi.fn().mockResolvedValue(Response.json({ created: true }));
    vi.stubGlobal('fetch', fetch);
    const body = new FormData();
    body.append('metadata', '{"description":"Example"}');
    await api('/api/v1/account/packages/example/publish', {
      method: 'POST',
      csrf_token: 'test-csrf',
      body,
    });
    const init = fetch.mock.calls[0][1];
    expect(init.body).toBe(body);
    expect(init.headers['Content-Type']).toBeUndefined();
    expect(init.headers['x-csrf-token']).toBe('test-csrf');
  });

  it('keeps actionable version conflicts but hides internal server details', async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(
        Response.json(
          { error: { code: 'conflict', message: 'Publish a new version.' } },
          { status: 409 },
        ),
      )
      .mockResolvedValueOnce(
        Response.json({ error: { message: 'Internal database detail' } }, { status: 500 }),
      );
    vi.stubGlobal('fetch', fetch);
    await expect(api('/api/v1/applets/example')).rejects.toMatchObject({
      message: 'Publish a new version.',
      status: 409,
      code: 'conflict',
    });
    await expect(api('/api/v1/applets/example')).rejects.toThrow('The Store is unavailable');
  });

  it('identifies expired sessions for a sign-in action and preserves aborted navigation', async () => {
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockResolvedValue(Response.json({ error: { message: 'Unauthorized' } }, { status: 401 })),
    );
    const error = await api('/api/v1/account/packages').catch((value: unknown) => value);
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({ status: 401 });
    const controller = new AbortController();
    controller.abort();
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(controller.signal.reason));
    await expect(api('/api/v1/applets', { signal: controller.signal })).rejects.toBe(
      controller.signal.reason,
    );
  });
});

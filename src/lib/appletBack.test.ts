import { describe, expect, it, vi } from 'vitest';
import { AppletBack } from './appletBack';
import { attachBridge } from './bridge';

const reply = (request_id: string, handled: unknown) => ({
  type: 'navigation_back_result',
  request_id,
  handled,
});

describe('applet Back bridge', () => {
  it('lets the current view handle one Back at a time and closes only if it declines', async () => {
    const postMessage = vi.fn();
    const port = {
      postMessage,
      start: vi.fn(),
      close: vi.fn(),
      onmessage: null,
    } as unknown as MessagePort;
    const call = vi.fn(async () => null);
    const back = new AppletBack(port);
    const dispose = attachBridge(port, call, back);
    const deliver = (data: unknown) => port.onmessage?.({ data } as MessageEvent);
    const close = vi.fn();

    back.request(close);
    back.request(close);
    expect(postMessage).toHaveBeenCalledExactlyOnceWith({
      type: 'navigation_back',
      request_id: '1',
    });
    await deliver(reply('2', false));
    await deliver(reply('1', 'false'));
    expect(close).not.toHaveBeenCalled();
    await deliver(reply('1', true));
    expect(close).not.toHaveBeenCalled();

    back.request(close);
    await deliver(reply('2', false));
    await deliver(reply('2', false));
    expect(close).toHaveBeenCalledOnce();
    expect(call).not.toHaveBeenCalled();
    dispose();
  });

  it('ignores delayed replies and does not close the view on timeout or disposal', () => {
    vi.useFakeTimers();
    try {
      const postMessage = vi.fn();
      const back = new AppletBack({ postMessage });
      const close = vi.fn();
      back.request(close);
      vi.advanceTimersByTime(1_000);
      back.request(close);
      back.receive(reply('1', false));
      expect(postMessage).toHaveBeenCalledTimes(2);
      expect(close).not.toHaveBeenCalled();
      back.dispose();
      back.receive(reply('2', false));
      back.request(close);
      expect(postMessage).toHaveBeenCalledTimes(2);
      expect(close).not.toHaveBeenCalled();
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });
});

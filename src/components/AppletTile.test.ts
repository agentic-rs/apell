import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AppletPress } from './AppletTile';

describe('applet icon gestures', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('lets an ordinary tap launch without opening actions later', () => {
    const press = new AppletPress();
    const actions = vi.fn();
    press.begin(20, 20, actions);
    vi.advanceTimersByTime(180);
    press.end();
    vi.runAllTimers();
    expect(actions).not.toHaveBeenCalled();
    expect(press.suppressClick).toBe(false);
  });

  it('opens actions for a steady hold and suppresses its release click only', () => {
    const press = new AppletPress();
    const actions = vi.fn();
    press.begin(20, 20, actions);
    press.move(22, 21);
    vi.advanceTimersByTime(500);
    press.end();
    expect(actions).toHaveBeenCalledOnce();
    expect(press.suppressClick).toBe(true);
    press.begin(20, 20, actions);
    press.end();
    expect(press.suppressClick).toBe(false);
  });

  it('cancels a scroll, even when the finger returns to the icon', () => {
    const press = new AppletPress();
    const actions = vi.fn();
    press.begin(20, 20, actions);
    press.move(20, 35);
    press.move(20, 20);
    vi.runAllTimers();
    press.end();
    expect(actions).not.toHaveBeenCalled();
    expect(press.suppressClick).toBe(true);
  });

  it('disarms an interrupted hold before its timer fires', () => {
    const press = new AppletPress();
    const actions = vi.fn();
    press.begin(20, 20, actions);
    press.cancel();
    vi.runAllTimers();
    expect(actions).not.toHaveBeenCalled();
    expect(press.suppressClick).toBe(true);
  });
});

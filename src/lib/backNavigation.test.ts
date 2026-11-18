import { describe, expect, it } from 'vitest';
import { BackActions } from './backNavigation';

describe('shell Back navigation', () => {
  it('dismisses the newest sheet first, then returns through the active page', () => {
    const navigation = new BackActions();
    const visited: string[] = [];
    const register = (priority: number, name: string) => {
      const remove = navigation.add(priority, () => {
        visited.push(name);
        remove();
      });
    };
    register(0, 'settings');
    register(100, 'sheet');
    register(25, 'editor');
    register(100, 'confirmation');
    for (let index = 0; index < 4; index++) expect(navigation.goBack()).toBe(true);
    expect(visited).toEqual(['confirmation', 'sheet', 'editor', 'settings']);
    expect(navigation.goBack()).toBe(false);
  });

  it('consumes Back at a busy sheet instead of closing the applet below it', () => {
    const navigation = new BackActions();
    let closed = false;
    navigation.add(50, () => {
      closed = true;
    });
    const dismiss = navigation.add(100, () => {});
    expect(navigation.goBack()).toBe(true);
    expect(closed).toBe(false);
    dismiss();
    expect(navigation.goBack()).toBe(true);
    expect(closed).toBe(true);
  });
});

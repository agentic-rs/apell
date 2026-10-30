import { describe, expect, it } from 'vitest';
import { assertJsonValue, MAX_JSON_DEPTH } from './json';

describe('configuration JSON validation', () => {
  it('allows null and shared plain subtrees without treating them as cycles', () => {
    const shared = { theme: 'dark', values: [null, true, false, 42, 0.5] };
    const record = Object.assign(Object.create(null), { shared });
    expect(() => assertJsonValue({ first: shared, second: shared, record })).not.toThrow();
    expect(() => assertJsonValue(null)).not.toThrow();
  });

  it('rejects values that would be coerced or lost by JSON transport', () => {
    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;
    const namedArray = Object.assign([1], { label: 'lost' });
    const hidden = Object.defineProperty({}, 'value', { value: 1 });
    const accessor = Object.defineProperty({}, 'value', {
      enumerable: true,
      get() {
        throw new Error('must not execute');
      },
    });
    for (const value of [
      NaN,
      Infinity,
      -Infinity,
      { nested: [NaN] },
      undefined,
      { nested: undefined },
      new Date(),
      new Map(),
      new Set(),
      new Uint8Array([1]),
      1n,
      () => 1,
      Symbol(),
      { [Symbol()]: 1 },
      new Array(2),
      namedArray,
      hidden,
      accessor,
      cyclic,
    ]) {
      expect(() => assertJsonValue(value)).toThrow(/plain JSON/);
    }
  });

  it('bounds nesting before traversing arbitrarily deep values', () => {
    let value: unknown = null;
    for (let level = 0; level < MAX_JSON_DEPTH; level += 1) value = [value];
    expect(() => assertJsonValue(value)).not.toThrow();
    expect(() => assertJsonValue([value])).toThrow(`at most ${MAX_JSON_DEPTH}`);
  });
});

import type { JsonValue } from './types';

export const MAX_JSON_DEPTH = 64;

/** Reject values that JSON transport would silently coerce, omit, or invoke. */
export function assertJsonValue(value: unknown): asserts value is JsonValue {
  const ancestors = new Set<object>();
  const invalid = () =>
    new TypeError(
      'Configuration values must be plain JSON with finite numbers and no undefined values, cycles, or custom objects.',
    );

  function visit(current: unknown, depth: number): void {
    if (current === null || typeof current === 'string' || typeof current === 'boolean') return;
    if (typeof current === 'number') {
      if (!Number.isFinite(current)) throw invalid();
      return;
    }
    if (typeof current !== 'object') throw invalid();
    if (depth >= MAX_JSON_DEPTH) {
      throw new TypeError(
        `Configuration values may nest at most ${MAX_JSON_DEPTH} objects or arrays.`,
      );
    }
    if (ancestors.has(current)) throw invalid();
    const array = Array.isArray(current);
    const prototype = Object.getPrototypeOf(current);
    if (
      array ? prototype !== Array.prototype : prototype !== Object.prototype && prototype !== null
    )
      throw invalid();
    const keys = Reflect.ownKeys(current);
    // JSON arrays have no holes or named properties; sparse arrays become nulls.
    if (array && keys.length !== current.length + 1) throw invalid();
    ancestors.add(current);
    try {
      for (const key of keys) {
        if (array && key === 'length') continue;
        if (typeof key !== 'string') throw invalid();
        if (
          array &&
          (!Number.isInteger(Number(key)) ||
            Number(key) < 0 ||
            Number(key) >= current.length ||
            String(Number(key)) !== key)
        )
          throw invalid();
        const descriptor = Object.getOwnPropertyDescriptor(current, key);
        if (!descriptor?.enumerable || !('value' in descriptor)) throw invalid();
        visit(descriptor.value, depth + 1);
      }
    } finally {
      ancestors.delete(current);
    }
  }

  visit(value, 0);
}

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  DEFAULT_BROWSE,
  readStorePreferences,
  rememberedStoreVersion,
  rememberStoreVersion,
  writeStorePreferences,
} from './cache';

const store_url = 'https://store.example.invalid';
const other = 'https://other.example.invalid';

describe('Store browsing preferences', () => {
  const storage = new Map<string, string>();
  beforeEach(() => {
    storage.clear();
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it('stores search and view preferences for the configured origin', () => {
    writeStorePreferences(store_url, { query: 'Sample', updates_only: true });
    expect(readStorePreferences(store_url)).toEqual({ query: 'Sample', updates_only: true });
    expect(readStorePreferences(other)).toEqual(DEFAULT_BROWSE);
  });

  it('remembers versions independently for each Store and applet', () => {
    rememberStoreVersion(store_url, 'sample', '1.0.0');
    rememberStoreVersion(other, 'sample', '2.0.0');
    rememberStoreVersion(other, 'next', '4.0.0');
    rememberStoreVersion(store_url, 'sample', '3.0.0');
    expect(rememberedStoreVersion(store_url, 'sample')).toBe('3.0.0');
    expect(rememberedStoreVersion(other, 'sample')).toBe('2.0.0');
    expect(rememberedStoreVersion(other, 'next')).toBe('4.0.0');
    rememberStoreVersion(other, 'sample');
    expect(rememberedStoreVersion(other, 'sample')).toBeUndefined();
  });

  it('replaces old version choices within a bounded list and skips corrupt storage', () => {
    for (let index = 0; index < 40; index++) {
      rememberStoreVersion(store_url, `sample-${index}`, '1.0.0');
    }
    expect(rememberedStoreVersion(store_url, 'sample-0')).toBeUndefined();
    expect(rememberedStoreVersion(store_url, 'sample-39')).toBe('1.0.0');
    for (const key of storage.keys()) storage.set(key, '{not-json');
    expect(rememberedStoreVersion(store_url, 'sample-39')).toBeUndefined();
    expect(readStorePreferences(store_url)).toEqual(DEFAULT_BROWSE);
  });
});

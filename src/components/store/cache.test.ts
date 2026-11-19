import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { StoreApplet, StoreCatalog, StoreDetail, StoreRelease } from '../../lib/types';
import {
  cachedCatalog,
  cachedDetail,
  cacheStoreCatalog,
  cacheStoreDetail,
  readStoreCache,
  writeStoreCache,
  writeStorePreferences,
} from './cache';

const store_url = 'https://store.example.invalid';
const release: StoreRelease = {
  version: '1.0.0',
  sha256: 'a'.repeat(64),
  size_bytes: 42,
  published_at: '2026-01-01T00:00:00Z',
  release_notes: 'What changed',
  api_version: 2,
  permissions: ['sqlite'],
  network: true,
  has_backend: false,
  download_url: '/api/v1/applets/sample/versions/1.0.0/download',
  prerelease: false,
};
const applet: StoreApplet = {
  applet_id: 'sample',
  name: 'Sample',
  description: 'A sample applet',
  project_url: null,
  icon_url: null,
  latest_release: release,
  latest_stable_release: release,
};
const installed = {
  schema_version: 1,
  api_version: 2,
  applet_id: 'sample',
  name: 'Local sample',
  version: '0.9.0',
  permissions: ['microphone'],
  network: true,
  sha256: 'b'.repeat(64),
  source: {
    kind: 'github' as const,
    repository_id: 1,
    repository: 'example/private-fixture',
    asset_name: 'sample.apel',
  },
};
const catalog: StoreCatalog = {
  store_url,
  applets: [{ ...applet, installed, update_available: true, source_conflict: true }],
};
const detail: StoreDetail = {
  store_url,
  applet,
  releases: [{ ...release, action: 'switch_source', downloaded_bytes: 12 }],
  installed,
  restart_required: true,
  switch_snapshot: { removed: false, source: installed.source, sha256: installed.sha256 },
};

describe('native Store browse cache', () => {
  const storage = new Map<string, string>();
  beforeEach(() => {
    storage.clear();
    vi.stubGlobal('localStorage', {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => storage.set(key, value),
    });
  });
  afterEach(() => vi.unstubAllGlobals());

  it('persists only public metadata and browse preferences for the configured origin', () => {
    const cache = readStoreCache(store_url);
    cacheStoreCatalog(cache, catalog);
    cacheStoreDetail(cache, detail, release.version);
    writeStoreCache(cache);
    writeStorePreferences(store_url, { query: 'Sample', updates_only: true, channel: 'nightly' });

    const saved = readStoreCache(store_url);
    const raw = [...storage.values()].join('');
    expect(raw).not.toMatch(
      /private-fixture|microphone|downloaded_bytes|switch_snapshot|restart_required/,
    );
    expect(saved.preferences).toEqual({ query: 'Sample', updates_only: true, channel: 'nightly' });
    expect(saved.details[0].selected_version).toBe(release.version);
    expect(cachedCatalog(saved)?.applets[0].installed).toBeNull();
    expect(cachedDetail(saved, applet.applet_id)).toMatchObject({
      installed: null,
      restart_required: false,
      releases: [{ action: 'conflict', release_notes: 'What changed' }],
    });
    const foreign = readStoreCache('https://other.example.invalid');
    expect(cachedCatalog(foreign)).toBeUndefined();
    expect(foreign.details).toEqual([]);
    expect(foreign.preferences.query).toBe('');
  });

  it('bounds its detail history and tolerates corrupted storage', () => {
    const cache = readStoreCache(store_url);
    for (let index = 0; index < 12; index++) {
      const next = { ...detail, applet: { ...applet, applet_id: `sample-${index}` } };
      cacheStoreDetail(cache, next, release.version);
    }
    writeStoreCache(cache);
    const saved = readStoreCache(store_url);
    expect(saved.details).toHaveLength(8);
    expect(saved.details[0].applet.applet_id).toBe('sample-11');
    expect(cachedDetail(saved, 'sample-0')).toBeUndefined();
    for (const key of storage.keys()) storage.set(key, '{not-json');
    expect(readStoreCache(store_url).details).toEqual([]);
  });
});

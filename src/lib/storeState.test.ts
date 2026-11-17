import { describe, expect, it } from 'vitest';
import { matchingStoreApplets, newStoreAccess, recommendedStoreRelease } from './storeState';
import type { InstalledApplet, StoreCatalogEntry, StoreDetail, StoreRelease } from './types';

const store_url = 'https://store.example.invalid';
const stable: StoreRelease = {
  version: '1.0.0',
  sha256: 'a'.repeat(64),
  size_bytes: 2048,
  published_at: '2026-01-01T00:00:00Z',
  release_notes: '',
  api_version: 1,
  permissions: ['llm', 'fs'],
  network: false,
  has_backend: false,
  download_url: '/api/downloads/chat/1.0.0.apel',
  prerelease: false,
};
const preview: StoreRelease = { ...stable, version: '2.0.0-beta.1', prerelease: true };
const installed: InstalledApplet = {
  schema_version: 1,
  api_version: 1,
  applet_id: 'chat',
  name: 'Chat',
  version: '1.0.0',
  sha256: stable.sha256,
  permissions: ['llm', 'fs'],
  network: false,
  source: { kind: 'store', store_url, applet_id: 'chat', channel: 'stable' },
};
const applet: StoreCatalogEntry = {
  applet_id: 'chat',
  name: 'Chat',
  description: 'A helpful companion',
  icon_url: null,
  project_url: null,
  latest_release: preview,
  latest_stable_release: stable,
  installed,
  update_available: true,
  source_conflict: false,
};
const detail: StoreDetail = {
  store_url,
  applet,
  installed,
  releases: [
    { ...preview, action: 'update' },
    { ...stable, action: 'installed' },
  ],
};

describe('store release review', () => {
  it('recommends stable for a new install, falling back to a preview when needed', () => {
    expect(recommendedStoreRelease({ ...detail, installed: null })).toBe(stable);
    expect(
      recommendedStoreRelease({
        ...detail,
        installed: null,
        applet: { ...applet, latest_stable_release: null },
      }),
    ).toBe(preview);
  });

  it('follows previews only for the installed applet’s own store source', () => {
    const preview_install: InstalledApplet = {
      ...installed,
      source: { kind: 'store', store_url, applet_id: 'chat', channel: 'preview' },
    };
    expect(recommendedStoreRelease(detail)).toBe(stable);
    expect(recommendedStoreRelease({ ...detail, installed: preview_install })).toBe(preview);
    expect(
      recommendedStoreRelease({
        ...detail,
        installed: preview_install,
        store_url: 'https://other.example.invalid',
      }),
    ).toBe(stable);
    expect(recommendedStoreRelease({ ...detail, installed: { ...installed, source: null } })).toBe(
      stable,
    );
  });

  it('calls out only added permissions and newly enabled network access', () => {
    const expanded = { ...stable, permissions: ['llm', 'camera', 'location'], network: true };
    expect(newStoreAccess(expanded, installed)).toEqual(['Camera', 'Location', 'Network access']);
    expect(newStoreAccess(stable, installed)).toEqual([]);
    expect(newStoreAccess(expanded, null)).toEqual([]);
    expect(newStoreAccess({ ...stable, network: false }, { ...installed, network: true })).toEqual(
      [],
    );
  });
});

describe('store catalog filtering', () => {
  it('shows upgrades only from the matching installation source', () => {
    const conflict = { ...applet, applet_id: 'external', source_conflict: true };
    const current = { ...applet, applet_id: 'current', update_available: false };
    expect(matchingStoreApplets([applet, conflict, current], '', true)).toEqual([applet]);
  });

  it('searches applet names and descriptions while keeping the update filter', () => {
    expect(matchingStoreApplets([applet], ' CHAT ', false)).toEqual([applet]);
    expect(matchingStoreApplets([applet], 'COMPANION', true)).toEqual([applet]);
    expect(matchingStoreApplets([applet], 'reader', false)).toEqual([]);
    expect(matchingStoreApplets([{ ...applet, update_available: false }], 'chat', true)).toEqual(
      [],
    );
  });
});

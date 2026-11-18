import { describe, expect, it } from 'vitest';
import {
  matchingStoreApplets,
  newStoreAccess,
  recommendedStoreRelease,
  storeChannelRelease,
} from './storeState';
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
const nightly: StoreRelease = { ...stable, version: '1.0.0-nightly.1', prerelease: true };
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
  latest_release: stable,
  latest_stable_release: stable,
  latest_nightly_release: nightly,
  installed,
  update_available: true,
  source_conflict: false,
};
const detail: StoreDetail = {
  store_url,
  applet,
  installed,
  restart_required: false,
  releases: [
    { ...stable, action: 'installed' },
    { ...nightly, action: 'older' },
  ],
};

describe('store release review', () => {
  it('recommends stable for a new install, falling back to a nightly-only release when needed', () => {
    expect(recommendedStoreRelease({ ...detail, installed: null })).toBe(stable);
    expect(
      recommendedStoreRelease({
        ...detail,
        installed: null,
        applet: { ...applet, latest_release: nightly, latest_stable_release: null },
      }),
    ).toBe(nightly);
  });

  it('follows Nightly only for the installed applet’s own store source', () => {
    const nightly_install: InstalledApplet = {
      ...installed,
      source: { kind: 'store', store_url, applet_id: 'chat', channel: 'nightly' },
    };
    expect(recommendedStoreRelease(detail)).toBe(stable);
    expect(recommendedStoreRelease({ ...detail, installed: nightly_install })).toBe(nightly);
    expect(recommendedStoreRelease(detail, 'nightly')).toBe(nightly);
    expect(
      recommendedStoreRelease({
        ...detail,
        installed: nightly_install,
        store_url: 'https://other.example.invalid',
      }),
    ).toBe(stable);
    expect(recommendedStoreRelease({ ...detail, installed: { ...installed, source: null } })).toBe(
      stable,
    );
  });

  it('does not treat the newest stable version as a nightly on an older Store', () => {
    const legacy = { ...applet, latest_nightly_release: undefined };
    expect(storeChannelRelease(legacy, 'nightly')).toBeNull();
    expect(storeChannelRelease({ ...legacy, latest_release: nightly }, 'nightly')).toBe(nightly);
    expect(recommendedStoreRelease({ ...detail, applet: legacy }, 'nightly').version).toBe(
      nightly.version,
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
  it('shows nightly-only applets after choosing Nightly, while Updates spans installed channels', () => {
    const nightly_only = { ...applet, latest_release: nightly, latest_stable_release: null };
    expect(matchingStoreApplets([nightly_only], '', false, 'stable')).toEqual([]);
    expect(matchingStoreApplets([nightly_only], '', false, 'nightly')).toEqual([nightly_only]);
    expect(matchingStoreApplets([nightly_only], '', true, 'stable')).toEqual([nightly_only]);
  });

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

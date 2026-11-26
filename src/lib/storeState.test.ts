import { describe, expect, it } from 'vitest';
import {
  matchingStoreApplets,
  newStoreAccess,
  recommendedStoreRelease,
  reviewSavedCatalog,
  reviewStoreRelease,
  storeChannelRelease,
} from './storeState';
import type {
  InstalledApplet,
  StoreCatalogEntry,
  StoreDetail,
  StorePendingDownload,
  StoreRelease,
} from './types';

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
  installation_unavailable: false,
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

  it('restores viewed releases only in the applet’s default channel', () => {
    const older_stable = { ...stable, version: '0.9.0', action: 'older' as const };
    const older_nightly = { ...nightly, version: '1.0.0-nightly.0', action: 'older' as const };
    const stable_detail = {
      ...detail,
      releases: [...detail.releases, older_stable, older_nightly],
    };
    expect(
      reviewStoreRelease(stable_detail, { remembered_version: older_stable.version }).release,
    ).toBe(older_stable);
    expect(
      reviewStoreRelease(stable_detail, { remembered_version: older_nightly.version }).release
        ?.version,
    ).toBe(stable.version);

    const retained_nightly = {
      ...stable_detail,
      installed: null,
      installed_channel: 'nightly' as const,
    };
    expect(
      reviewStoreRelease(retained_nightly, { remembered_version: older_nightly.version }).release,
    ).toBe(older_nightly);
    expect(
      reviewStoreRelease(retained_nightly, { remembered_version: older_stable.version }).release
        ?.version,
    ).toBe(nightly.version);
  });

  it('keeps an explicit preview while following the matching installation channel by default', () => {
    const cached = {
      ...detail,
      installed: {
        ...installed,
        source: {
          kind: 'store' as const,
          store_url,
          applet_id: 'chat',
          channel: 'nightly' as const,
        },
      },
    };
    expect(reviewStoreRelease(cached, { remembered_version: stable.version })).toMatchObject({
      channel: 'nightly',
      release: { version: nightly.version },
    });
    expect(reviewStoreRelease(cached, { channel: 'stable' })).toMatchObject({
      channel: 'stable',
      release: { version: stable.version },
    });
    expect(reviewStoreRelease(cached, { version: stable.version }, true).release?.version).toBe(
      nightly.version,
    );
    expect(
      reviewStoreRelease({ ...cached, store_url: 'https://other.example.invalid' }).channel,
    ).toBe('stable');
  });

  it('keeps a channel selected when its releases are missing from the cached details', () => {
    const nightly_detail = {
      ...detail,
      installed_channel: 'nightly' as const,
      releases: [detail.releases[0]],
    };
    expect(reviewStoreRelease(nightly_detail)).toEqual({
      channel: 'nightly',
      installed_channel: 'nightly',
      versions: [],
      recommended: undefined,
      release: undefined,
    });
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

describe('paused Store download review', () => {
  const paused = {
    ...stable,
    version: '2.0.0',
    sha256: 'b'.repeat(64),
    action: 'update' as const,
    downloaded_bytes: 1024,
  };
  const latest = {
    ...paused,
    version: '3.0.0',
    sha256: 'c'.repeat(64),
    downloaded_bytes: undefined,
  };
  const pending_download: StorePendingDownload = {
    origin: store_url,
    applet_id: applet.applet_id,
    name: applet.name,
    version: paused.version,
    channel: 'stable',
    sha256: paused.sha256,
    size_bytes: paused.size_bytes,
    downloaded_bytes: paused.downloaded_bytes,
  };
  const current = {
    ...detail,
    applet: { ...applet, latest_release: latest, latest_stable_release: latest },
    releases: [latest, paused, ...detail.releases],
  };

  it('keeps the exact paused update selected when an update check recommends a newer version', () => {
    expect(reviewStoreRelease(current, {}, true).release).toBe(latest);
    expect(reviewStoreRelease(current, { pending_download }, true).release).toBe(paused);
  });

  it('does not substitute a release when the saved identity or progress no longer matches', () => {
    for (const change of [
      { version: '2.0.1' },
      { sha256: latest.sha256 },
      { channel: 'nightly' as const },
      { size_bytes: paused.size_bytes + 1 },
      { origin: 'https://other.example.invalid' },
      { applet_id: 'other' },
    ]) {
      expect(
        reviewStoreRelease(current, { pending_download: { ...pending_download, ...change } })
          .release,
      ).toBeUndefined();
    }
    expect(
      reviewStoreRelease(
        { ...current, releases: [{ ...paused, downloaded_bytes: undefined }] },
        { pending_download },
      ).release,
    ).toBeUndefined();
  });

  it('keeps a paused channel switch on the installed Store and requires that source', () => {
    const switch_release = { ...paused, prerelease: true, action: 'switch_channel' as const };
    const choice = { pending_download: { ...pending_download, channel: 'nightly' as const } };
    const switch_detail = { ...current, releases: [switch_release, ...current.releases] };
    expect(reviewStoreRelease(switch_detail, choice, true).release).toBe(switch_release);
    expect(
      reviewStoreRelease(
        { ...switch_detail, installed: { ...installed, source: null } },
        choice,
        true,
      ).release,
    ).toBeUndefined();
  });
});

describe('store catalog filtering', () => {
  it('joins saved listings to the verified local inventory without carrying over update claims', () => {
    const nightly_install: InstalledApplet = {
      ...installed,
      source: { kind: 'store', store_url, applet_id: 'chat', channel: 'nightly' },
    };
    const other_origin = { ...applet, applet_id: 'external' };
    const mismatched_id = { ...applet, applet_id: 'different-id' };
    const unavailable = { ...applet, applet_id: 'broken' };
    const removed = { ...applet, applet_id: 'removed' };
    const catalog = {
      store_url,
      applets: [applet, other_origin, mismatched_id, unavailable, removed],
    };
    const report = {
      applets: [
        nightly_install,
        {
          ...nightly_install,
          applet_id: 'external',
          source: {
            kind: 'store' as const,
            channel: 'nightly' as const,
            store_url: 'https://other.example.invalid',
            applet_id: 'external',
          },
        },
        { ...nightly_install, applet_id: 'different-id' },
      ],
      unavailable: [{ applet_id: 'broken', error: 'fixture package read error' }],
    };
    const reviewed = reviewSavedCatalog(catalog, report);
    expect(
      reviewed.applets.map(
        ({ applet_id, installed, source_conflict, installation_unavailable }) => [
          applet_id,
          installed?.applet_id,
          source_conflict,
          installation_unavailable,
        ],
      ),
    ).toEqual([
      ['chat', 'chat', false, false],
      ['external', 'external', true, false],
      ['different-id', 'different-id', true, false],
      ['broken', undefined, true, true],
      ['removed', undefined, false, false],
    ]);
    expect(reviewed.applets.every((entry) => !entry.update_available)).toBe(true);
    expect(recommendedStoreRelease({ ...detail, installed: reviewed.applets[0].installed })).toBe(
      nightly,
    );
    expect(applet.update_available).toBe(true);
    expect(reviewSavedCatalog(reviewed).applets[0].installed).toBeNull();
  });

  it('includes Stable and Nightly-only applets in one catalog', () => {
    const nightly_only = {
      ...applet,
      applet_id: 'nightly-only',
      latest_release: nightly,
      latest_stable_release: null,
    };
    const catalog = [applet, nightly_only];
    expect(matchingStoreApplets(catalog, '', false)).toEqual(catalog);
    expect(matchingStoreApplets(catalog, '', true)).toEqual(catalog);
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

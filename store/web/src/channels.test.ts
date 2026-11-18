import { describe, expect, it } from 'vitest';
import { channelForVersion, channelPath, latestInChannel, selectedChannel } from './channels';
import type { Applet, Release } from './types';

const release = (version: string): Release => ({
  version,
  sha256: 'a'.repeat(64),
  size_bytes: 10,
  published_at: '2026-01-01T00:00:00Z',
  release_notes: '',
  api_version: 2,
  permissions: [],
  network: true,
  has_backend: false,
  download_url: `/api/v1/applets/test/versions/${version}/download`,
  prerelease: channelForVersion(version) === 'nightly',
});

const stable = release('2.0.0');
const nightly = release('1.9.0-nightly.3');
const applet: Applet = {
  applet_id: 'test',
  name: 'Test',
  description: 'A test applet',
  project_url: null,
  icon_url: null,
  latest_release: stable,
  latest_stable_release: stable,
};

describe('Store website release channels', () => {
  it('browses Nightly independently of the higher Stable release', () => {
    const listing = { ...applet, latest_nightly_release: nightly };
    expect(latestInChannel(listing, 'stable')).toBe(stable);
    expect(latestInChannel(listing, 'nightly')).toBe(nightly);
    expect(latestInChannel({ ...listing, latest_stable_release: null }, 'stable')).toBeUndefined();
    expect(
      latestInChannel({ ...listing, latest_nightly_release: null }, 'nightly'),
    ).toBeUndefined();
  });

  it('does not cross channels when an older server omits its latest Nightly', () => {
    expect(latestInChannel(applet, 'nightly')).toBeUndefined();
    expect(latestInChannel(applet, 'nightly', [stable, nightly])).toBe(nightly);
    expect(latestInChannel({ ...applet, latest_release: nightly }, 'nightly')).toBe(nightly);
  });

  it('defaults links to Stable and preserves opt-in on a Nightly link', () => {
    const origin = 'https://store.example';
    expect(selectedChannel(new URL('/applets/test?channel=preview', origin))).toBe('stable');
    expect(selectedChannel(new URL(channelPath('/applets/test', 'nightly'), origin))).toBe(
      'nightly',
    );
    expect(channelForVersion('2.0.0+build-with-hyphens')).toBe('stable');
  });
});

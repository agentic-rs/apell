import type { Applet, Release } from './types';

export type ReleaseChannel = 'stable' | 'nightly';

export const channel_labels = { stable: 'Stable', nightly: 'Nightly' } as const;

export function getReleaseChannel(release: Release): ReleaseChannel {
  return release.prerelease ? 'nightly' : 'stable';
}

export function channelForVersion(version: string): ReleaseChannel {
  return version.split('+', 1)[0].includes('-') ? 'nightly' : 'stable';
}

export function catalogRelease(applet: Applet): Release {
  return applet.latest_stable_release ?? applet.latest_nightly_release ?? applet.latest_release;
}

export function latestInChannel(
  applet: Applet,
  channel: ReleaseChannel,
  versions: readonly Release[] = [],
): Release | undefined {
  const latest =
    channel === 'stable' ? applet.latest_stable_release : applet.latest_nightly_release;
  if (latest !== undefined) return latest ?? undefined;
  // Older servers expose only the highest version across both channels.
  // When available, the detail response lists all releases in precedence order.
  return (
    versions.find((release) => getReleaseChannel(release) === channel) ??
    (getReleaseChannel(applet.latest_release) === channel ? applet.latest_release : undefined)
  );
}

export function selectedChannel(url: URL): ReleaseChannel {
  return url.searchParams.get('channel') === 'nightly' ? 'nightly' : 'stable';
}

export function availableChannel(
  requested: ReleaseChannel,
  releases: readonly Release[],
): ReleaseChannel {
  if (releases.some((release) => getReleaseChannel(release) === requested)) return requested;
  const other = requested === 'stable' ? 'nightly' : 'stable';
  return releases.some((release) => getReleaseChannel(release) === other) ? other : requested;
}

export function setChannel(url: URL, channel: ReleaseChannel): void {
  if (channel === 'nightly') url.searchParams.set('channel', channel);
  else url.searchParams.delete('channel');
}

export function channelPath(path: string, channel: ReleaseChannel): string {
  return channel === 'nightly' ? `${path}?channel=nightly` : path;
}

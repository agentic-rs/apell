import type {
  AppletListReport,
  StoreCatalog,
  StoreCatalogEntry,
  StoreApplet,
  StoreChannel,
  StoreDetail,
  StorePendingDownload,
  StoreRelease,
  InstalledApplet,
  PackageSource,
} from './types';

const permission_names: Record<string, string> = {
  sqlite: 'Local database',
  llm: 'AI models',
  resources: 'Downloads',
  device: 'Device information',
  http: 'Network requests',
  fs: 'Private files',
  config: 'Preferences',
  websocket: 'Live connections',
  secrets: 'Secure storage',
  crypto: 'Secure randomness',
  runtime: 'Background tasks',
  audio: 'Microphone and audio',
  speech: 'Speech transcription',
  camera: 'Camera',
  location: 'Location',
  photos: 'Photo library',
};

export const permissionName = (permission: string) => permission_names[permission] ?? permission;

export function storeSourceLabel(source: PackageSource | null): string {
  if (source?.kind === 'github') return `GitHub (${source.repository})`;
  if (source?.kind === 'store') return source.store_url;
  return 'a local file';
}

export function matchingStoreApplets(
  applets: StoreCatalogEntry[],
  query: string,
  updates_only: boolean,
): StoreCatalogEntry[] {
  const search = query.trim().toLocaleLowerCase();
  return applets.filter(
    (applet) =>
      (!updates_only || (applet.update_available && !applet.source_conflict)) &&
      `${applet.name} ${applet.description}`.toLocaleLowerCase().includes(search),
  );
}

export const storeChannelName = (channel: StoreChannel): string =>
  channel === 'nightly' ? 'Nightly' : 'Stable';

export const storeReleaseChannel = (release: StoreRelease): StoreChannel =>
  release.prerelease ? 'nightly' : 'stable';

export function storeChannelRelease(
  applet: StoreApplet,
  channel: StoreChannel,
): StoreRelease | null {
  if (channel === 'stable') return applet.latest_stable_release;
  return applet.latest_nightly_release === undefined
    ? applet.latest_release.prerelease
      ? applet.latest_release
      : null
    : applet.latest_nightly_release;
}

export function installedStoreChannel(
  store_url: string,
  applet_id: string,
  installed: InstalledApplet | null,
): StoreChannel | undefined {
  const source = installed?.source;
  return source?.kind === 'store' &&
    source.store_url === store_url &&
    source.applet_id === applet_id
    ? source.channel
    : undefined;
}

export function reviewSavedCatalog(
  catalog: StoreCatalog,
  report?: Pick<AppletListReport, 'applets' | 'unavailable'>,
): StoreCatalog {
  const installed_applets = new Map(report?.applets.map((applet) => [applet.applet_id, applet]));
  const unavailable = new Set(report?.unavailable.map((applet) => applet.applet_id));
  return {
    store_url: catalog.store_url,
    applets: catalog.applets.map((applet) => {
      const installation_unavailable = unavailable.has(applet.applet_id);
      const installed = installation_unavailable
        ? null
        : (installed_applets.get(applet.applet_id) ?? null);
      return {
        ...applet,
        installed,
        update_available: false,
        source_conflict:
          installation_unavailable ||
          Boolean(
            installed && !installedStoreChannel(catalog.store_url, applet.applet_id, installed),
          ),
        installation_unavailable,
      };
    }),
  };
}

export function recommendedStoreRelease(
  detail: Pick<StoreDetail, 'store_url' | 'applet' | 'installed' | 'installed_channel'> &
    Partial<Pick<StoreDetail, 'releases'>>,
  requested?: StoreChannel,
): StoreRelease {
  const { applet } = detail;
  const channel =
    requested ??
    detail.installed_channel ??
    installedStoreChannel(detail.store_url, applet.applet_id, detail.installed) ??
    'stable';
  return (
    storeChannelRelease(applet, channel) ??
    (channel === 'nightly' && applet.latest_nightly_release === undefined
      ? detail.releases?.find((release) => release.prerelease)
      : undefined) ??
    storeChannelRelease(applet, channel === 'nightly' ? 'stable' : 'nightly') ??
    applet.latest_release
  );
}

export interface StoreReleaseChoice {
  version?: string;
  channel?: StoreChannel;
  remembered_version?: string;
  pending_download?: StorePendingDownload;
}

export function reviewStoreRelease(
  detail: StoreDetail,
  choice: StoreReleaseChoice = {},
  installed_source = false,
) {
  const installed_channel =
    detail.installed_channel ??
    installedStoreChannel(detail.store_url, detail.applet.applet_id, detail.installed);
  const pending = choice.pending_download;
  if (pending) {
    const release =
      detail.store_url === pending.origin &&
      detail.applet.applet_id === pending.applet_id &&
      (!installed_source || installed_channel)
        ? detail.releases.find(
            (item) =>
              item.version === pending.version &&
              item.sha256 === pending.sha256 &&
              item.size_bytes === pending.size_bytes &&
              storeReleaseChannel(item) === pending.channel &&
              (item.downloaded_bytes ?? 0) > 0,
          )
        : undefined;
    return {
      channel: pending.channel,
      installed_channel,
      versions: release ? [release] : [],
      recommended: undefined,
      release,
    };
  }
  const explicit = installed_source
    ? undefined
    : detail.releases.find((release) => release.version === choice.version);
  const channel = explicit
    ? storeReleaseChannel(explicit)
    : ((installed_source ? undefined : choice.channel) ??
      installed_channel ??
      storeReleaseChannel(recommendedStoreRelease(detail)));
  const versions = detail.releases.filter((release) => storeReleaseChannel(release) === channel);
  const latest = recommendedStoreRelease(detail, channel);
  const recommended = versions.find((release) => release.version === latest.version);
  const remembered = installed_source
    ? undefined
    : versions.find((release) => release.version === choice.remembered_version);
  return {
    channel,
    installed_channel,
    versions,
    recommended,
    release:
      installed_source && !installed_channel
        ? undefined
        : (explicit ?? remembered ?? recommended ?? versions[0]),
  };
}

export function newStoreAccess(release: StoreRelease, installed: InstalledApplet | null): string[] {
  if (!installed) return [];
  const added = release.permissions
    .filter((permission) => !installed.permissions.includes(permission))
    .map(permissionName);
  if (release.network && !installed.network) added.push('Network access');
  return added;
}

export function storeSize(bytes: number): string {
  return bytes < 1024 * 1024
    ? `${Math.max(1, Math.round(bytes / 1024))} KB`
    : `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export function storeDate(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.valueOf())
    ? ''
    : date.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
}

import type {
  StoreCatalogEntry,
  StoreDetail,
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

export function recommendedStoreRelease(
  detail: Pick<StoreDetail, 'store_url' | 'applet' | 'installed'>,
): StoreRelease {
  return detail.installed?.source?.kind === 'store' &&
    detail.installed.source.store_url === detail.store_url &&
    detail.installed.source.applet_id === detail.applet.applet_id &&
    detail.installed.source.channel === 'preview'
    ? detail.applet.latest_release
    : (detail.applet.latest_stable_release ?? detail.applet.latest_release);
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

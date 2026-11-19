// Public listing fields shared by the Store website and the native shell.
export interface StoreRelease {
  version: string;
  sha256: string;
  size_bytes: number;
  published_at: string;
  release_notes: string;
  api_version: number;
  permissions: string[];
  network: boolean;
  has_backend: boolean;
  download_url: string;
  prerelease: boolean;
}

export interface StoreApplet {
  applet_id: string;
  name: string;
  description: string;
  project_url: string | null;
  icon_url: string | null;
  latest_release: StoreRelease;
  latest_stable_release: StoreRelease | null;
  latest_nightly_release?: StoreRelease | null;
}

function invalid(): never {
  throw new Error('Invalid public Store metadata.');
}

export function publicRecord(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return invalid();
  return value as Record<string, unknown>;
}

function string(value: unknown, limit: number): string {
  return typeof value === 'string' && value.length <= limit ? value : invalid();
}

function nullableString(value: unknown, limit: number): string | null {
  return value === null ? null : string(value, limit);
}

function boolean(value: unknown): boolean {
  return typeof value === 'boolean' ? value : invalid();
}

export function publicInteger(value: unknown): number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : invalid();
}

export function publicArray(value: unknown, limit: number): unknown[] {
  return Array.isArray(value) && value.length <= limit ? value : invalid();
}

// Explicit projection keeps native review state and unexpected fields out of disk caches.
export function publicStoreRelease(value: unknown): StoreRelease {
  const row = publicRecord(value);
  const sha256 = string(row.sha256, 64);
  const size_bytes = publicInteger(row.size_bytes);
  if (!/^[0-9a-f]{64}$/.test(sha256) || size_bytes === 0) return invalid();
  return {
    version: string(row.version, 80),
    sha256,
    size_bytes,
    published_at: string(row.published_at, 64),
    release_notes: string(row.release_notes, 10_000),
    api_version: publicInteger(row.api_version),
    permissions: publicArray(row.permissions, 100).map((value) => string(value, 100)),
    network: boolean(row.network),
    has_backend: boolean(row.has_backend),
    download_url: string(row.download_url, 2048),
    prerelease: boolean(row.prerelease),
  };
}

export function publicStoreApplet(value: unknown, include_release_notes = true): StoreApplet {
  const row = publicRecord(value);
  const applet_id = string(row.applet_id, 80);
  const icon_url = nullableString(row.icon_url, 2048);
  if (!/^[a-z0-9_-]{1,80}$/.test(applet_id)) return invalid();
  const release = (value: unknown) => {
    const entry = publicStoreRelease(value);
    return include_release_notes ? entry : { ...entry, release_notes: '' };
  };
  return {
    applet_id,
    name: string(row.name, 100),
    description: string(row.description, 2000),
    project_url: nullableString(row.project_url, 2048),
    icon_url,
    latest_release: release(row.latest_release),
    latest_stable_release:
      row.latest_stable_release === null ? null : release(row.latest_stable_release),
    ...(row.latest_nightly_release !== undefined
      ? {
          latest_nightly_release:
            row.latest_nightly_release === null ? null : release(row.latest_nightly_release),
        }
      : {}),
  };
}

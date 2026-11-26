const PREFERENCES_KEY = 'apell.store.preferences.v1';
const RELEASES_KEY = 'apell.store.releases.v1';
const MAX_RECENT_RELEASES = 32;

export interface StoreBrowsePreferences {
  query: string;
  updates_only: boolean;
}

export const DEFAULT_BROWSE: StoreBrowsePreferences = {
  query: '',
  updates_only: false,
};

export function readStorePreferences(store_url: string): StoreBrowsePreferences {
  try {
    const raw = localStorage.getItem(PREFERENCES_KEY);
    if (!raw || raw.length > 4_096) return DEFAULT_BROWSE;
    const data = JSON.parse(raw);
    if (data?.store_url !== store_url) return DEFAULT_BROWSE;
    return {
      query: typeof data.query === 'string' ? data.query.slice(0, 200) : '',
      updates_only: data.updates_only === true,
    };
  } catch {
    return DEFAULT_BROWSE;
  }
}

export function writeStorePreferences(store_url: string, preferences: StoreBrowsePreferences) {
  try {
    localStorage.setItem(PREFERENCES_KEY, JSON.stringify({ store_url, ...preferences }));
  } catch {
    // Browsing preferences are best effort.
  }
}

interface RememberedRelease {
  store_url: string;
  applet_id: string;
  version: string;
}

function rememberedReleases(): RememberedRelease[] {
  try {
    const raw = localStorage.getItem(RELEASES_KEY);
    if (!raw || raw.length > 80_000) return [];
    const data = JSON.parse(raw);
    if (!Array.isArray(data) || data.length > MAX_RECENT_RELEASES) return [];
    return data.filter(
      (entry) =>
        typeof entry?.store_url === 'string' &&
        entry.store_url.length <= 2_048 &&
        typeof entry.applet_id === 'string' &&
        entry.applet_id.length <= 80 &&
        typeof entry.version === 'string' &&
        entry.version.length <= 80,
    );
  } catch {
    return [];
  }
}

export function rememberedStoreVersion(store_url: string, applet_id: string): string | undefined {
  return rememberedReleases().find(
    (entry) => entry.store_url === store_url && entry.applet_id === applet_id,
  )?.version;
}

export function rememberStoreVersion(store_url: string, applet_id: string, version?: string) {
  try {
    const previous = rememberedReleases();
    const index = previous.findIndex(
      (entry) => entry.store_url === store_url && entry.applet_id === applet_id,
    );
    if (!version && index < 0) return;
    if (index >= 0) previous.splice(index, 1);
    if (version) previous.unshift({ store_url, applet_id, version });
    localStorage.setItem(RELEASES_KEY, JSON.stringify(previous.slice(0, MAX_RECENT_RELEASES)));
  } catch {
    // Remembered releases are hints for selecting versions in the reviewed Store response.
  }
}

import {
  publicArray,
  publicRecord,
  publicStoreApplet,
  publicStoreRelease,
} from '../../../packages/store-catalog';
import type { StoreApplet, StoreCatalog, StoreDetail, StoreRelease } from '../../lib/types';

const CACHE_KEY = 'apell.store.browse.v1';
const PREFERENCES_KEY = 'apell.store.preferences.v1';
const MAX_CACHE_LENGTH = 1_500_000;
const MAX_DETAILS = 8;
const MAX_DETAIL_LENGTH = 200_000;

export interface StoreBrowsePreferences {
  query: string;
  updates_only: boolean;
}

export const DEFAULT_BROWSE: StoreBrowsePreferences = {
  query: '',
  updates_only: false,
};

interface CachedDetail {
  applet: StoreApplet;
  releases: StoreRelease[];
  selected_version: string;
}

export interface StoreBrowseCache {
  store_url: string;
  preferences: StoreBrowsePreferences;
  catalog?: StoreApplet[];
  details: CachedDetail[];
}

function publicDetail(value: unknown): CachedDetail {
  const entry = publicRecord(value);
  const releases = publicArray(entry.releases, 1024);
  if (!releases.length || typeof entry.selected_version !== 'string')
    throw new Error('Invalid cached applet details');
  return {
    applet: publicStoreApplet(entry.applet),
    releases: releases.map(publicStoreRelease),
    selected_version: entry.selected_version,
  };
}

function readPreferences(store_url: string): StoreBrowsePreferences {
  try {
    const raw = localStorage.getItem(PREFERENCES_KEY);
    if (!raw || raw.length > 4_096) return DEFAULT_BROWSE;
    const data = publicRecord(JSON.parse(raw));
    if (data.store_url !== store_url) return DEFAULT_BROWSE;
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

export function readStoreCache(store_url: string): StoreBrowseCache {
  const preferences = readPreferences(store_url);
  const empty = { store_url, preferences, details: [] };
  try {
    const raw = localStorage.getItem(CACHE_KEY);
    if (!raw || raw.length > MAX_CACHE_LENGTH) return empty;
    const data = publicRecord(JSON.parse(raw));
    if (data.store_url !== store_url) return empty;
    return {
      store_url,
      preferences,
      ...(Array.isArray(data.catalog) && data.catalog.length <= 256
        ? { catalog: data.catalog.map((entry) => publicStoreApplet(entry, false)) }
        : {}),
      details: Array.isArray(data.details)
        ? data.details.slice(0, MAX_DETAILS).map(publicDetail)
        : [],
    };
  } catch {
    return empty;
  }
}

export function writeStoreCache(cache: StoreBrowseCache): void {
  try {
    // A quota error or storage restriction must not break Store navigation.
    const details = cache.details.slice(0, MAX_DETAILS);
    const data = { store_url: cache.store_url, catalog: cache.catalog, details };
    let snapshot = JSON.stringify(data);
    while (details.length && snapshot.length > MAX_CACHE_LENGTH) {
      details.pop();
      snapshot = JSON.stringify(data);
    }
    if (snapshot.length <= MAX_CACHE_LENGTH) localStorage.setItem(CACHE_KEY, snapshot);
  } catch {
    // Public cache is best effort.
  }
}

export function cacheStoreCatalog(cache: StoreBrowseCache, catalog: StoreCatalog): void {
  if (cache.store_url !== catalog.store_url) return;
  try {
    cache.catalog = catalog.applets.map((entry) => publicStoreApplet(entry, false));
  } catch {
    // A cache projection error must not affect a live catalog.
  }
}

export function cacheStoreDetail(
  cache: StoreBrowseCache,
  detail: StoreDetail,
  selected_version: string,
): void {
  if (cache.store_url !== detail.store_url) return;
  try {
    const entry = publicDetail({ ...detail, selected_version });
    const others = cache.details.filter(
      (existing) => existing.applet.applet_id !== entry.applet.applet_id,
    );
    cache.details =
      JSON.stringify(entry).length <= MAX_DETAIL_LENGTH
        ? [entry, ...others].slice(0, MAX_DETAILS)
        : others;
  } catch {
    // A cache projection error must not affect a live detail review.
  }
}

export function cachedCatalog(cache: StoreBrowseCache): StoreCatalog | undefined {
  return (
    cache.catalog && {
      store_url: cache.store_url,
      applets: cache.catalog.map((applet) => ({
        ...applet,
        installed: null,
        update_available: false,
        source_conflict: false,
      })),
    }
  );
}

export function cachedDetail(cache: StoreBrowseCache, applet_id: string): StoreDetail | undefined {
  const entry = cache.details.find((existing) => existing.applet.applet_id === applet_id);
  return (
    entry && {
      store_url: cache.store_url,
      applet: entry.applet,
      releases: entry.releases.map((release) => ({ ...release, action: 'conflict' })),
      installed: null,
      restart_required: false,
    }
  );
}

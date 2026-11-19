import { api, ApiError } from './api';
import type { Applet, AppletDetails } from './types';
import {
  publicStoreApplet,
  publicStoreRelease,
  publicRecord,
  publicArray,
  publicInteger,
} from '../../../packages/store-catalog';

export type PublicData = { applets: Applet[] } | AppletDetails;
export const PUBLIC_CACHE_KEY = 'apell.public-cache.v1';
export const FRESH_MS = 60_000;
export const RETAIN_MS = 7 * 24 * 60 * 60_000;
const MAX_ENTRIES = 16;
const MAX_BYTES = 2 * 1024 * 1024;
const MAX_ENTRY_BYTES = 512 * 1024;
interface Entry {
  saved_at: number;
  data: PublicData;
}
interface StorageAccess {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

export function publicEndpoint(path: string): boolean {
  return path === '/api/v1/applets' || /^\/api\/v1\/applets\/[a-z0-9_-]{1,80}$/.test(path);
}

// Project only the catalog contract, so extra response fields never enter durable storage.
export function publicData(path: string, value: unknown): PublicData {
  if (!publicEndpoint(path)) throw new Error('Only public catalog responses may be cached.');
  const row = publicRecord(value);
  if (path === '/api/v1/applets')
    return { applets: publicArray(row.applets, 256).map((value) => publicStoreApplet(value)) };
  const result = {
    applet: publicStoreApplet(row.applet),
    releases: publicArray(row.releases, 1024).map(publicStoreRelease),
  };
  if (path !== `/api/v1/applets/${result.applet.applet_id}`)
    throw new Error('Invalid public Store response.');
  return result;
}

export class PublicCache {
  private entries = new Map<string, Entry>();
  private pending = new Map<string, Promise<PublicData>>();
  private generations = new Map<string, number>();
  constructor(
    private storage: StorageAccess | undefined,
    private clock: () => number = Date.now,
  ) {
    try {
      const raw = storage?.getItem(PUBLIC_CACHE_KEY);
      if (!raw || raw.length * 2 > MAX_BYTES) return;
      for (const [path, value] of Object.entries(publicRecord(JSON.parse(raw)))) {
        if (this.entries.size >= MAX_ENTRIES || !publicEndpoint(path)) continue;
        const row = publicRecord(value);
        const saved_at = publicInteger(row.saved_at);
        if (saved_at > this.clock() || this.clock() - saved_at > RETAIN_MS) continue;
        const data = publicData(path, row.data);
        if (JSON.stringify(data).length * 2 <= MAX_ENTRY_BYTES)
          this.entries.set(path, { saved_at, data });
      }
    } catch {
      this.entries.clear();
    }
  }

  read(path: string): { data: PublicData; fresh: boolean } | undefined {
    const entry = this.entries.get(path);
    if (!entry) return;
    const age = this.clock() - entry.saved_at;
    if (age < 0 || age > RETAIN_MS) {
      this.entries.delete(path);
      return;
    }
    return { data: entry.data, fresh: age < FRESH_MS };
  }

  load(path: string, force = false): Promise<PublicData> {
    if (!publicEndpoint(path))
      return Promise.reject(new Error('Only public catalog responses may be cached.'));
    const existing = this.read(path);
    if (!force && existing?.fresh) return Promise.resolve(existing.data);
    const running = this.pending.get(path);
    if (running) return running;
    const generation = this.generations.get(path) ?? 0;
    const work = api<unknown>(path, { signal: AbortSignal.timeout(15_000) })
      .then((value) => {
        if ((this.generations.get(path) ?? 0) !== generation)
          throw new DOMException('Catalog changed.', 'AbortError');
        const data = publicData(path, value);
        this.entries.set(path, { saved_at: this.clock(), data });
        this.persist();
        return data;
      })
      .catch((error) => {
        if (
          error instanceof ApiError &&
          error.status === 404 &&
          (this.generations.get(path) ?? 0) === generation
        ) {
          this.entries.delete(path);
          this.persist();
        }
        throw error;
      })
      .finally(() => {
        if (this.pending.get(path) === work) this.pending.delete(path);
      });
    this.pending.set(path, work);
    return work;
  }

  invalidate(applet_id: string): void {
    for (const path of ['/api/v1/applets', `/api/v1/applets/${applet_id}`]) {
      this.generations.set(path, (this.generations.get(path) ?? 0) + 1);
      this.entries.delete(path);
      this.pending.delete(path);
    }
    this.persist();
  }

  private persist(): void {
    const entries = [...this.entries.entries()]
      .filter(([, entry]) => this.clock() - entry.saved_at <= RETAIN_MS)
      .sort((a, b) => b[1].saved_at - a[1].saved_at);
    const kept: Record<string, Entry> = {};
    let count = 0;
    let size = 4;
    for (const [path, entry] of entries) {
      const bytes = JSON.stringify({ [path]: entry }).length * 2;
      if (bytes > MAX_ENTRY_BYTES || size + bytes > MAX_BYTES || count >= MAX_ENTRIES) continue;
      kept[path] = entry;
      size += bytes;
      count += 1;
    }
    this.entries = new Map(Object.entries(kept));
    try {
      this.storage?.setItem(PUBLIC_CACHE_KEY, JSON.stringify(kept));
    } catch {
      /* Storage quotas/private browsing do not block online browsing. */
    }
  }
}

let storage: StorageAccess | undefined;
try {
  storage = globalThis.localStorage;
} catch {
  /* Best effort durable cache. */
}
export const publicCache = new PublicCache(storage);
export const PUBLIC_INVALIDATED = 'apell-public-invalidated';
export function invalidatePublicPackage(applet_id: string): void {
  publicCache.invalidate(applet_id);
  window.dispatchEvent(new CustomEvent(PUBLIC_INVALIDATED, { detail: { applet_id } }));
}

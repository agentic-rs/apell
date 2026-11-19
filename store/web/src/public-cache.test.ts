import { afterEach, describe, expect, it, vi } from 'vitest';
import { FRESH_MS, PUBLIC_CACHE_KEY, PublicCache, RETAIN_MS } from './public-cache';

const release = {
  version: '1.0.0',
  sha256: 'a'.repeat(64),
  size_bytes: 100,
  published_at: '2026-01-01T00:00:00Z',
  release_notes: '',
  api_version: 2,
  permissions: [],
  network: true,
  has_backend: false,
  download_url: '/api/v1/applets/example/versions/1.0.0/download',
  prerelease: false,
};
const applet = {
  applet_id: 'example',
  name: 'Example',
  description: 'An applet',
  project_url: null,
  icon_url: null,
  latest_release: release,
  latest_stable_release: release,
};
function storage() {
  const values = new Map<string, string>();
  return {
    values,
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => {
      values.set(key, value);
    },
  };
}
const catalog = '/api/v1/applets';
const detail = `${catalog}/example`;
afterEach(() => vi.unstubAllGlobals());

describe('public catalog cache', () => {
  it('persists only projected public fields, reuses fresh snapshots, and expires stale fallback', async () => {
    let now = 100_000;
    const disk = storage();
    const fetch = vi.fn().mockResolvedValue(
      Response.json({
        applets: [{ ...applet, owner: 'private', csrf_token: 'private' }],
        user: 'private',
      }),
    );
    vi.stubGlobal('fetch', fetch);
    const cache = new PublicCache(disk, () => now);
    await cache.load(catalog);
    const restored = new PublicCache(disk, () => now);
    expect(restored.read(catalog)?.fresh).toBe(true);
    await restored.load(catalog);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(disk.getItem(PUBLIC_CACHE_KEY)).not.toContain('private');
    now += FRESH_MS;
    expect(restored.read(catalog)?.fresh).toBe(false);
    now += RETAIN_MS;
    expect(restored.read(catalog)).toBeUndefined();
  });

  it('rejects authenticated endpoints without sending or persisting them', async () => {
    const fetch = vi.fn();
    vi.stubGlobal('fetch', fetch);
    const disk = storage();
    const cache = new PublicCache(disk);
    for (const endpoint of [
      '/api/v1/auth/me',
      '/api/v1/account/packages',
      '/api/v1/applets/example/versions/1.0.0/download',
    ]) {
      await expect(cache.load(endpoint)).rejects.toThrow('Only public');
    }
    expect(fetch).not.toHaveBeenCalled();
    expect(disk.values.size).toBe(0);
  });

  it('deduplicates concurrent requests and ignores responses predating an invalidation', async () => {
    const disk = storage();
    const cache = new PublicCache(disk);
    let finish!: (response: Response) => void;
    const fetch = vi
      .fn()
      .mockImplementationOnce(
        () =>
          new Promise<Response>((resolve) => {
            finish = resolve;
          }),
      )
      .mockResolvedValueOnce(Response.json({ applet, releases: [release] }));
    vi.stubGlobal('fetch', fetch);
    const old = cache.load(detail);
    expect(cache.load(detail)).toBe(old);
    cache.invalidate('example');
    await cache.load(detail);
    finish(Response.json({ applet: { ...applet, name: 'Outdated' }, releases: [release] }));
    await expect(old).rejects.toMatchObject({ name: 'AbortError' });
    expect(cache.read(detail)?.data).toMatchObject({ applet: { name: 'Example' } });
  });

  it('retains saved public data on network failures but drops definitive missing packages', async () => {
    const cache = new PublicCache(storage());
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(Response.json({ applet, releases: [release] }))
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce(Response.json({ error: { message: 'Missing' } }, { status: 404 }));
    vi.stubGlobal('fetch', fetch);
    await cache.load(detail);
    await expect(cache.load(detail, true)).rejects.toThrow('connection');
    expect(cache.read(detail)).toBeDefined();
    await expect(cache.load(detail, true)).rejects.toMatchObject({ status: 404 });
    expect(cache.read(detail)).toBeUndefined();
  });

  it('bounds saved entries and tolerates corrupt or unavailable storage', async () => {
    let now = 100_000;
    const disk = storage();
    disk.setItem(PUBLIC_CACHE_KEY, '{bad');
    const cache = new PublicCache(disk, () => now);
    const fetch = vi.fn().mockImplementation((path: string) =>
      Promise.resolve(
        Response.json({
          applet: { ...applet, applet_id: path.split('/').at(-1) },
          releases: [release],
        }),
      ),
    );
    vi.stubGlobal('fetch', fetch);
    for (let i = 0; i < 20; i += 1) {
      now += 1;
      await cache.load(`${catalog}/sample-${i}`);
    }
    expect(Object.keys(JSON.parse(disk.getItem(PUBLIC_CACHE_KEY)!))).toHaveLength(16);
    expect(cache.read(`${catalog}/sample-0`)).toBeUndefined();
    const blocked = new PublicCache({
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('quota');
      },
    });
    await expect(blocked.load(detail)).resolves.toMatchObject({ applet: { applet_id: 'example' } });
    expect(blocked.read(detail)).toBeDefined();
  });

  it('refuses mismatched package identities and malformed persisted responses', async () => {
    const disk = storage();
    disk.setItem(
      PUBLIC_CACHE_KEY,
      JSON.stringify({
        [catalog]: { saved_at: Date.now(), data: { applets: [{ applet_id: 'bad' }] } },
      }),
    );
    const cache = new PublicCache(disk);
    expect(cache.read(catalog)).toBeUndefined();
    vi.stubGlobal(
      'fetch',
      vi
        .fn()
        .mockResolvedValue(
          Response.json({ applet: { ...applet, applet_id: 'other' }, releases: [release] }),
        ),
    );
    await expect(cache.load(detail)).rejects.toThrow('Invalid public');
    expect(cache.read(detail)).toBeUndefined();
  });

  it('omits oversized snapshots instead of letting storage grow without bound', async () => {
    const disk = storage();
    const cache = new PublicCache(disk);
    const large_releases = Array.from({ length: 100 }, () => ({
      ...release,
      release_notes: 'x'.repeat(10_000),
    }));
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(Response.json({ applet, releases: large_releases })),
    );
    await expect(cache.load(detail)).resolves.toMatchObject({ applet });
    expect(cache.read(detail)).toBeUndefined();
    expect(disk.getItem(PUBLIC_CACHE_KEY)!.length).toBeLessThan(2 * 1024 * 1024);
  });
});

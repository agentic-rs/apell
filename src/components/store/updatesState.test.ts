import { describe, expect, it } from 'vitest';
import type { StoreUpdateEntry, StoreUpdateSource } from '../../lib/types';
import { applyUpdateCheck, savePreviousUpdates, verifySavedUpdates } from './updatesState';

const first_store = 'https://alpha.example.invalid';
const second_store = 'https://beta.example.invalid';

function offer(applet_id: string, store_url: string): StoreUpdateEntry {
  return {
    applet: {
      applet_id,
      api_version: 2,
      schema_version: 1,
      name: applet_id,
      version: '1.0.0',
      sha256: 'a'.repeat(64),
      network: true,
      permissions: [],
      source: { kind: 'store', store_url, applet_id, channel: 'stable' },
    },
    release: {
      version: '2.0.0',
      sha256: 'b'.repeat(64),
      size_bytes: 1000,
      published_at: '2026-01-01T00:00:00Z',
      release_notes: '',
      api_version: 2,
      permissions: [],
      network: true,
      has_backend: false,
      download_url: `/api/v1/applets/${applet_id}/versions/2.0.0/download`,
      prerelease: false,
    },
  };
}

const journal = offer('journal', first_store);
const canvas = offer('canvas', second_store);
const timer = offer('timer', second_store);
const initial = {
  sources: [
    { store_url: first_store, installed_count: 1, checked_count: 1, updates: [journal] },
    { store_url: second_store, installed_count: 2, checked_count: 2, updates: [canvas, timer] },
  ],
};

describe('Store update check results', () => {
  it('keeps previous results visibly saved when checking again', () => {
    const latest = applyUpdateCheck(initial);
    const saved = savePreviousUpdates(latest)!;
    expect(saved.fresh).toBe(false);
    expect(saved.sources[0].updates).toEqual([]);
    expect(saved.sources[0].saved_updates).toEqual([journal]);
    expect(savePreviousUpdates(saved)).toEqual(saved);
    expect(latest.sources[0].updates).toEqual([journal]);
  });

  it('retains saved rows from a failed origin only for exact verified installations', () => {
    const saved = savePreviousUpdates(applyUpdateCheck(initial));
    const unreachable: StoreUpdateSource = {
      store_url: second_store,
      installed_count: 2,
      checked_count: 0,
      updates: [],
      error: 'The Store is unavailable.',
      unreachable_installations: [
        { applet_id: canvas.applet.applet_id, sha256: canvas.applet.sha256, channel: 'stable' },
        { applet_id: timer.applet.applet_id, sha256: timer.applet.sha256, channel: 'nightly' },
      ],
    };
    const partial = applyUpdateCheck({ sources: [initial.sources[0], unreachable] }, saved);
    expect(partial.fresh).toBe(true);
    expect(partial.sources[0]).toMatchObject({ updates: [journal], saved_updates: [] });
    expect(partial.sources[1]).toMatchObject({ updates: [], saved_updates: [canvas] });

    const recovered = applyUpdateCheck(
      {
        sources: [{ ...unreachable, checked_count: 1, error: undefined }],
      },
      partial,
    );
    expect(recovered.sources).toHaveLength(1);
    expect(recovered.sources[0].saved_updates).toEqual([]);
  });

  it('drops removed, changed-source, and replaced copies from saved results', () => {
    const saved = savePreviousUpdates(applyUpdateCheck(initial));
    const verified = verifySavedUpdates(saved, {
      applets: [
        { ...journal.applet, sha256: 'c'.repeat(64) },
        {
          ...canvas.applet,
          source: { kind: 'store', store_url: first_store, applet_id: 'canvas', channel: 'stable' },
        },
      ],
      unavailable: [],
      restart_required: [],
    });
    expect(verified?.sources.every((source) => source.saved_updates.length === 0)).toBe(true);
  });
});

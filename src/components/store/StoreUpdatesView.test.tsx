import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { StoreUpdateEntry, StoreUpdateResults } from '../../lib/types';
import { StoreCatalogView } from './StoreCatalogView';

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
      removal_revision: 'b'.repeat(64),
      network: true,
      permissions: [],
      source: { kind: 'store', store_url, applet_id, channel: 'stable' },
    },
    version: '2.0.0',
  };
}

const saved = {
  store_url: first_store,
  installed_count: 1,
  checked_count: 0,
  updates: [],
  saved_updates: [offer('journal', first_store)],
};

function render(updates?: StoreUpdateResults, loading = false, error = '') {
  const noop = () => {};
  return renderToStaticMarkup(
    <StoreCatalogView
      store_url={second_store}
      updates={updates}
      download_count={0}
      loading={loading}
      error={error}
      query=""
      updates_only
      onQuery={noop}
      onFilter={noop}
      onRefresh={noop}
      onRetryUpdateSource={noop}
      onAddress={noop}
      onSelect={noop}
      onSelectUpdate={noop}
      onOpenDownloads={noop}
    />,
  );
}

describe('Store update check results', () => {
  it('shows locally verified saved hints while checking without a fresh update badge', () => {
    const html = render({ fresh: false, sources: [saved] }, true);
    expect(html).toContain('Checking installed applets for updates');
    expect(html).toContain('Check saved release for journal');
    expect(html).toContain('Saved result');
    expect(html).not.toContain('Updates (');
  });

  it('shows a retained live update as a saved result while checking again', () => {
    const sources = [
      {
        store_url: second_store,
        installed_count: 1,
        checked_count: 1,
        updates: [offer('canvas', second_store)],
        saved_updates: [],
      },
    ];
    const checking = render({ fresh: false, sources }, true);
    expect(checking).toContain('Check saved release for canvas');
    expect(checking).toContain('Saved result');
    expect(checking).not.toContain('Review update for canvas');
    expect(checking).not.toContain('Updates (1)');

    const current = render({ fresh: true, sources });
    expect(current).toContain('Review update for canvas');
    expect(current).toContain('Updates (1)');
    expect(current).not.toContain('Saved result');
  });

  it('counts only live updates when one Store is offline', () => {
    const html = render({
      fresh: true,
      sources: [
        { ...saved, error: 'This Store is unavailable.' },
        {
          store_url: second_store,
          installed_count: 1,
          checked_count: 1,
          updates: [offer('canvas', second_store)],
          saved_updates: [],
        },
      ],
    });
    expect(html).toContain('This Store is unavailable.');
    expect(html).toContain(`aria-label="Retry updates from ${first_store}"`);
    expect(html).not.toContain(`aria-label="Retry updates from ${second_store}"`);
    expect(html).toContain('Check saved release for journal');
    expect(html).toContain('Review update for canvas');
    expect(html).toContain('Updates (1)');
    expect(html).not.toContain('Updates (2)');
  });

  it('distinguishes a failed whole check from a result with no updates', () => {
    const html = render({ fresh: false, sources: [saved] }, false, 'Check failed');
    expect(html).toContain('Showing updates from previous checks.');
    expect(html).toContain('Try again');
    expect(html).not.toContain('No updates available');
    expect(render(undefined, false, 'Check failed')).toContain('Update status is unknown.');
  });
});

import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { ConfigSyncConflict } from '../lib/types';
import { ConfigSyncReview } from './ConfigSyncReview';

function render(cloud_apply_blocker: ConfigSyncConflict['cloud_apply_blocker']) {
  const conflict: ConfigSyncConflict = {
    state: 'conflict',
    conflict_id: 'fixture-review',
    status: {
      state: 'ready',
      last_synced_at: null,
      local_changes: true,
      pending_conflict_id: 'fixture-review',
      syncing: false,
      sync_error: null,
    },
    local: {
      connections: [
        { connection_id: 'local', name: 'Local copy', provider_id: 'openai', model_id: 'fixture' },
      ],
      preferred_connection_id: null,
    },
    cloud: { connections: [], preferred_connection_id: null },
    cloud_apply_blocker,
  };
  return renderToStaticMarkup(
    <ConfigSyncReview
      conflict={conflict}
      providers={[]}
      busy={false}
      disabled={false}
      stale={false}
      error=""
      onRefresh={() => {}}
      onKeepLocal={() => {}}
      onUseCloud={() => {}}
    />,
  );
}

describe('cloud choice in model settings review', () => {
  it('names the local collision and disables the cloud choice only', () => {
    const markup = render({ kind: 'destination_conflict', name: 'Device-only fixture' });
    expect(markup).toContain('“Device-only fixture” conflicts with a cloud connection.');
    expect(markup).toMatch(/<button[^>]*disabled[^>]*>Use cloud on this device<\/button>/);
    expect(markup).toContain('<button type="button">Sync this device to cloud</button>');
    expect(markup).toContain('<button type="button">Refresh review</button>');
  });

  it('explains how many slots to free and restores the choice in an unblocked review', () => {
    expect(render({ kind: 'connection_limit', extra_connections: 2 })).toContain(
      'Free 2 slots in Model connections',
    );
    expect(render({ kind: 'connection_limit', extra_connections: 1 })).toContain(
      'Free 1 slot in Model connections',
    );

    const markup = render(null);
    expect(markup).not.toContain('disabled=""');
    expect(markup).toContain('and their saved credentials will be removed.');
  });
});

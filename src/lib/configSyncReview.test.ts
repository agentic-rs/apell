import { describe, expect, it } from 'vitest';
import { compareConfigSync } from './configSyncReview';
import type { ConfigSyncConnection, ConfigSyncSummary } from './types';

function connection(
  connection_id: string,
  name: string,
  model_id = 'model-one',
): ConfigSyncConnection {
  return { connection_id, name, provider_id: 'openai', model_id };
}

function snapshot(
  connections: ConfigSyncConnection[],
  preferred_connection_id: string | null = null,
): ConfigSyncSummary {
  return { connections, preferred_connection_id };
}

describe('model settings conflict comparison', () => {
  it('pairs edits, counts device-only connections and preserves the order of review rows', () => {
    const unchanged = connection('shared', 'Shared');
    const local_edit = connection('editing', 'Writing');
    const cloud_edit = { ...local_edit, model_id: 'model-two' };
    const device_only = connection('device', 'Device only');
    const cloud_only = connection('cloud', 'Cloud only');

    const result = compareConfigSync(
      snapshot([unchanged, device_only, local_edit]),
      snapshot([cloud_only, { ...unchanged }, cloud_edit]),
    );

    expect(result.changes).toEqual([
      { local: device_only },
      { local: local_edit, cloud: cloud_edit },
      { cloud: cloud_only },
    ]);
    expect(result.removed_count).toBe(1);
    expect(result.preferred).toBeUndefined();
    expect(result.ambiguous_ids.size).toBe(0);
  });

  it('distinguishes different connection IDs that share a name, provider and model', () => {
    const local = connection('local', 'Writing');
    const cloud = { ...local, connection_id: 'cloud' };
    const similar = connection('similar', 'Writing', 'model-two');

    const result = compareConfigSync(
      snapshot([local, similar], local.connection_id),
      snapshot([cloud, similar], cloud.connection_id),
    );

    expect(result.changes).toEqual([{ local }, { cloud }]);
    expect(result.removed_count).toBe(1);
    expect(result.preferred).toEqual({ local, cloud });
    expect(result.ambiguous_ids).toEqual(new Set(['local', 'cloud']));
  });

  it('detects a preference-only switch between otherwise identical connections', () => {
    const first = connection('first', 'Writing');
    const second = { ...first, connection_id: 'second' };
    const connections = [first, second];

    const result = compareConfigSync(
      snapshot(connections, 'first'),
      snapshot(connections, 'second'),
    );

    expect(result.changes).toEqual([]);
    expect(result.removed_count).toBe(0);
    expect(result.preferred).toEqual({ local: first, cloud: second });
    expect(result.ambiguous_ids).toEqual(new Set(['first', 'second']));
  });

  it('shows preferred changes when the same connection is edited or a preference is cleared', () => {
    const before = connection('preferred', 'Default');
    const after = { ...before, name: 'Updated', provider_id: 'deepseek', model_id: 'model-two' };
    const id = before.connection_id;

    expect(compareConfigSync(snapshot([before], id), snapshot([after], id)).preferred).toEqual({
      local: before,
      cloud: after,
    });
    const cleared = compareConfigSync(snapshot([before], id), snapshot([before]));
    expect(cleared.preferred).toEqual({ local: before });
    expect(cleared.changes).toEqual([]);
  });
});

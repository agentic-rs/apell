import { describe, expect, it } from 'vitest';
import type { ConfigSyncStatus, KeySyncStatus } from '../lib/types';
import { accountSyncAttention } from './SettingsMenu';

const model: ConfigSyncStatus = {
  state: 'ready',
  last_synced_at: null,
  local_changes: false,
  pending_conflict_id: null,
  syncing: false,
  sync_error: null,
};

function verifiedKeys(
  sync_state: Extract<KeySyncStatus, { state: 'verified' }>['sync_state'],
): KeySyncStatus {
  return { state: 'verified', sync_state, sync_error: null };
}

describe('Settings Account attention', () => {
  it('prioritizes model review and key conflicts over retryable failures', () => {
    const failed_model = { ...model, sync_error: 'Private diagnostic text' };
    expect(
      accountSyncAttention(
        { ...failed_model, pending_conflict_id: 'conflict' },
        verifiedKeys('conflict'),
      ),
    ).toBe('Review model changes');
    expect(accountSyncAttention(failed_model, verifiedKeys('conflict'))).toBe('Key sync conflict');
    expect(accountSyncAttention(failed_model, verifiedKeys('failed'))).toBe(
      'Couldn’t sync model settings',
    );
  });

  it('summarizes only actionable health for verified keys', () => {
    expect(accountSyncAttention(model, verifiedKeys('failed'))).toBe('Couldn’t sync keys');
    expect(accountSyncAttention(model, verifiedKeys('paused'))).toBe('Key sync paused');
    expect(accountSyncAttention(model, verifiedKeys('waiting_for_model_settings'))).toBe(
      'Keys need model settings',
    );
    expect(accountSyncAttention(model, verifiedKeys('pending'))).toBeUndefined();
    expect(accountSyncAttention(model, verifiedKeys('syncing'))).toBeUndefined();
    expect(accountSyncAttention(model, verifiedKeys('current'))).toBeUndefined();
    expect(accountSyncAttention(model, { state: 'unverified' })).toBeUndefined();
  });

  it('keeps opted-out, healthy, and in-progress model sync quiet', () => {
    expect(accountSyncAttention({ ...model, state: 'off' })).toBeUndefined();
    expect(accountSyncAttention({ ...model, state: 'disabled' })).toBeUndefined();
    expect(accountSyncAttention({ ...model, local_changes: true })).toBeUndefined();
    expect(
      accountSyncAttention({ ...model, syncing: true, sync_error: 'Previous attempt failed' }),
    ).toBeUndefined();
  });
});

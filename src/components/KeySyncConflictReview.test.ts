import { describe, expect, it } from 'vitest';
import type { KeySyncConflict, KeySyncConflictResolution } from '../lib/types';
import { keyConflictConfirmation } from './KeySyncConflictReview';

const conflict: KeySyncConflict = {
  connection_id: 'connection',
  review_id: 'review',
  name: 'Provider fixture',
  provider_id: 'openai',
  model_id: 'fixture-model',
  device_has_key: true,
  cloud_has_key: true,
};

describe('key sync conflict confirmations', () => {
  type Case = {
    resolution: KeySyncConflictResolution['resolution'];
    device_has_key: boolean;
    cloud_has_key: boolean;
    action: string;
    description: string;
    removes: boolean;
  };

  const cases: Case[] = [
    {
      resolution: 'keep_device',
      device_has_key: true,
      cloud_has_key: true,
      action: 'Replace cloud key',
      description: 'Replace the saved cloud key with the key on this device.',
      removes: false,
    },
    {
      resolution: 'use_cloud',
      device_has_key: true,
      cloud_has_key: true,
      action: 'Replace device key',
      description: 'Replace the saved key on this device with the cloud key.',
      removes: false,
    },
    {
      resolution: 'keep_device',
      device_has_key: false,
      cloud_has_key: true,
      action: 'Remove cloud key',
      description: 'This device has no saved key. Remove the saved key from your Account.',
      removes: true,
    },
    {
      resolution: 'use_cloud',
      device_has_key: true,
      cloud_has_key: false,
      action: 'Remove device key',
      description: 'The cloud has no saved key. Remove the saved key from this device.',
      removes: true,
    },
    {
      resolution: 'keep_device',
      device_has_key: true,
      cloud_has_key: false,
      action: 'Save to cloud',
      description: 'Save the key on this device to your Account.',
      removes: false,
    },
    {
      resolution: 'use_cloud',
      device_has_key: false,
      cloud_has_key: true,
      action: 'Save to device',
      description: 'Save the cloud key to this device.',
      removes: false,
    },
  ];

  it.each(cases)('explains “$action” before applying the choice', (entry) => {
    const { resolution, device_has_key, cloud_has_key, ...expected } = entry;
    expect(
      keyConflictConfirmation({
        conflict: { ...conflict, device_has_key, cloud_has_key },
        resolution,
      }),
    ).toEqual(expected);
  });
});

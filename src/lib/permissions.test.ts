import { describe, expect, it } from 'vitest';
import { appletPermissionRows } from './permissions';

describe('applet permission summary', () => {
  it('shows package access even without managed grants', () => {
    expect(
      appletPermissionRows({
        network: false,
        permissions: ['llm', 'secrets', 'fs', 'photos'],
        has_lib: true,
      }),
    ).toEqual([
      { kind: 'fixed', key: 'network', label: 'Network', status: 'Off' },
      { kind: 'fixed', key: 'llm', label: 'Shared AI', status: 'Off' },
      { kind: 'fixed', key: 'secrets', label: 'Secure storage', status: 'On' },
      { kind: 'fixed', key: 'fs', label: 'App files', status: 'On' },
      { kind: 'fixed', key: 'photos', label: 'Photos', status: 'Pick each time' },
    ]);
  });

  it('shows prerequisites for capture and backend services', () => {
    expect(
      appletPermissionRows({ network: true, permissions: ['camera', 'secrets'], has_lib: false }),
    ).toEqual([
      { kind: 'fixed', key: 'network', label: 'Network', status: 'On' },
      { kind: 'managed', key: 'camera', label: 'Camera', unavailable_reason: 'Requires app files' },
      { kind: 'fixed', key: 'secrets', label: 'Secure storage', status: 'Requires applet backend' },
    ]);
  });
});

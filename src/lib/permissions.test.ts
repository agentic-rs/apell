import { describe, expect, it } from 'vitest';
import { appletPermissionRows, groupDevicePermissions, systemPermissionLabel } from './permissions';
import type { AppletPermission, ManagedPermission, PermissionGrant } from './types';

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
      { kind: 'picker', key: 'photos', label: 'Photos', status: 'Pick each time' },
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

  it('shows a separate system label only for authorization still needed or blocked', () => {
    expect(systemPermissionLabel('blocked')).toBe('Blocked by system');
    expect(systemPermissionLabel('restricted')).toBe('Restricted by system');
    expect(systemPermissionLabel('not_granted')).toBe('System access needed');
    expect(systemPermissionLabel('not_determined')).toBe('System permission not requested');
    expect(systemPermissionLabel('allowed')).toBeUndefined();
    expect(systemPermissionLabel('unknown')).toBeUndefined();
  });
});

function deviceGrant(permission: ManagedPermission, overrides: Partial<PermissionGrant> = {}) {
  return {
    permission,
    access: 'remembered',
    mode: 'remembered',
    available: true,
    system_authorization: 'allowed',
    ...overrides,
  } satisfies PermissionGrant;
}

function fixtureApplet(
  applet_id: string,
  permissions: string[] | null,
  grants: PermissionGrant[],
): AppletPermission {
  return {
    applet_id,
    name: applet_id,
    installed_revision: permissions ? 'verified-package' : undefined,
    manifest: permissions ? { permissions, network: true, has_lib: false } : null,
    grants,
    restart_required: false,
  };
}

describe('permission groups', () => {
  it('includes every managed permission in display order and preserves applet order within it', () => {
    const camera = deviceGrant('camera');
    const location = deviceGrant('location');
    const first = fixtureApplet('lens-notes', ['camera', 'location', 'fs'], [location, camera]);
    const stale = deviceGrant('camera', { access: 'previous_package', mode: 'ask' });
    const second = fixtureApplet('daily-photos', ['camera', 'fs'], [stale]);

    expect(groupDevicePermissions([first, second])).toEqual([
      { permission: 'microphone', entries: [], allowed_count: 0 },
      {
        permission: 'camera',
        entries: [
          { applet: first, grant: camera },
          { applet: second, grant: stale },
        ],
        allowed_count: 1,
      },
      { permission: 'location', entries: [{ applet: first, grant: location }], allowed_count: 1 },
    ]);
  });

  it('counts only usable remembered declarations, independently of system authorization', () => {
    const applets = [
      fixtureApplet(
        'reading-camera',
        ['camera', 'fs'],
        [deviceGrant('camera', { system_authorization: 'blocked' })],
      ),
      fixtureApplet(
        'camera-restricted',
        ['camera', 'fs'],
        [deviceGrant('camera', { system_authorization: 'restricted' })],
      ),
      fixtureApplet(
        'unsupported-camera',
        ['camera', 'fs'],
        [deviceGrant('camera', { available: false })],
      ),
      fixtureApplet('no-private-files', ['camera'], [deviceGrant('camera')]),
      fixtureApplet(
        'removed-declaration',
        ['fs'],
        [deviceGrant('camera', { access: 'undeclared', mode: undefined })],
      ),
      fixtureApplet('unverified-package', null, [
        deviceGrant('camera', { access: 'unverified_package', mode: undefined }),
      ]),
      fixtureApplet(
        'ask-first',
        ['camera', 'fs'],
        [deviceGrant('camera', { access: 'ask', mode: 'ask' })],
      ),
      fixtureApplet(
        'block-camera',
        ['camera', 'fs'],
        [deviceGrant('camera', { access: 'blocked', mode: 'blocked' })],
      ),
      fixtureApplet('recorder', ['audio'], [deviceGrant('microphone')]),
      fixtureApplet('picker', ['fs', 'photos'], []),
    ];

    const [microphone, camera, location] = groupDevicePermissions(applets);
    expect(camera.entries.map((entry) => entry.applet)).toEqual(applets.slice(0, 8));
    expect(camera.allowed_count).toBe(2);
    expect(microphone.entries.map((entry) => entry.applet)).toEqual([applets[8]]);
    expect(microphone.allowed_count).toBe(1);
    expect(location.entries).toEqual([]);
    expect(location.allowed_count).toBe(0);
  });
});

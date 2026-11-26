import { describe, expect, it } from 'vitest';
import {
  appletPermissionRows,
  groupDevicePermissions,
  groupSystemPermission,
  matchingPermissionApplets,
  systemPermissionLabel,
} from './permissions';
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
    grant_revision: 'reviewed-grant',
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
  it('summarizes a device block for current usable grants only', () => {
    const blocked = deviceGrant('camera', {
      system_authorization: 'blocked',
      can_open_system_settings: true,
    });
    const ask = { ...blocked, mode: 'ask' as const, access: 'previous_package' as const };
    const ignored = [
      fixtureApplet('blocked-in-apell', ['camera', 'fs'], [{ ...blocked, mode: 'blocked' }]),
      fixtureApplet('unavailable-platform', ['camera', 'fs'], [{ ...blocked, available: false }]),
      fixtureApplet('missing-files', ['camera'], [blocked]),
      fixtureApplet('undeclared', ['fs'], [blocked]),
      fixtureApplet('unavailable-applet', null, [blocked]),
    ];
    const entries = (applets: AppletPermission[]) => groupDevicePermissions(applets)[1].entries;
    expect(groupSystemPermission(entries(ignored))).toBeUndefined();
    expect(
      groupSystemPermission(
        entries([
          ...ignored,
          fixtureApplet('ask-current-package', ['camera', 'fs'], [ask]),
          fixtureApplet('always-allow', ['camera', 'fs'], [blocked]),
        ]),
      ),
    ).toEqual({ authorization: 'blocked', can_open_system_settings: true });
  });

  it('preserves platform capability and avoids summarizing conflicting system states', () => {
    const entries = (...grants: PermissionGrant[]) =>
      grants.map((grant, index) => ({
        grant,
        applet: fixtureApplet(`camera-${index}`, ['camera', 'fs'], [grant]),
      }));
    const grant = (
      system_authorization: PermissionGrant['system_authorization'],
      can_open_system_settings: boolean,
    ) => deviceGrant('camera', { system_authorization, can_open_system_settings });
    expect(groupSystemPermission(entries(grant('not_granted', true)))).toEqual({
      authorization: 'not_granted',
      can_open_system_settings: true,
    });
    expect(groupSystemPermission(entries(grant('blocked', false)))).toEqual({
      authorization: 'blocked',
      can_open_system_settings: false,
    });
    expect(groupSystemPermission(entries(grant('restricted', true)))).toEqual({
      authorization: 'restricted',
      can_open_system_settings: false,
    });
    for (const state of ['allowed', 'not_determined', 'unknown'] as const) {
      expect(groupSystemPermission(entries(grant(state, false)))).toBeUndefined();
      expect(
        groupSystemPermission(entries(grant('blocked', true), grant(state, false))),
      ).toBeUndefined();
    }
  });

  it('filters display names and IDs without excluding saved permissions for damaged packages', () => {
    const first = { ...fixtureApplet('journal', ['fs'], []), name: 'Café Journal' };
    const damaged = {
      ...fixtureApplet('journal-offline', null, [deviceGrant('camera')]),
      name: first.name,
    };
    const applets = [first, damaged, fixtureApplet('other', [], [])];
    expect(matchingPermissionApplets(applets, '   ')).toEqual(applets);
    expect(matchingPermissionApplets(applets, '  CAFE\u0301 ')).toEqual([first, damaged]);
    expect(matchingPermissionApplets(applets, ' JOURNAL-OFFLINE ')).toEqual([damaged]);
    expect(matchingPermissionApplets(applets, 'camera')).toEqual([]);
  });

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

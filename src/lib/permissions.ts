import type {
  AppletPermission,
  AppletPermissionManifest,
  ManagedPermission,
  PermissionGrant,
  PermissionMode,
  SystemAuthorization,
} from './types';

export const managed_permission_labels: Record<ManagedPermission, string> = {
  microphone: 'Microphone',
  camera: 'Camera',
  location: 'Location',
};

export const permission_mode_labels: Record<PermissionMode, string> = {
  ask: 'Ask every time',
  remembered: 'Always allow',
  blocked: 'Don’t allow',
};

export function isPermissionMode(value: string): value is PermissionMode {
  return Object.hasOwn(permission_mode_labels, value);
}

export const device_access_note = 'Apell’s approvals still depend on your system permissions.';
export const permission_unavailable = 'Unavailable on this platform';

const system_access_labels: Partial<Record<SystemAuthorization, string>> = {
  blocked: 'Blocked by system',
  restricted: 'Restricted by system',
  not_granted: 'System access needed',
  not_determined: 'System permission not requested',
};

export function systemPermissionLabel(authorization: SystemAuthorization) {
  return system_access_labels[authorization];
}

const permission_labels: Record<string, string> = {
  sqlite: 'Saved app data',
  llm: 'Shared AI',
  resources: 'Downloaded resources',
  device: 'Device information',
  http: 'Network requests',
  fs: 'App files',
  config: 'App preferences',
  websocket: 'Live connections',
  secrets: 'Secure storage',
  crypto: 'Secure randomness',
  runtime: 'Timers',
  audio: 'Audio files and playback',
  speech: 'Speech transcription',
};

const offline_status: Record<string, string> = {
  http: 'Off',
  websocket: 'Off',
  llm: 'Off',
  resources: 'Local only',
  speech: 'On-device only',
};

const backend_permissions = new Set(['websocket', 'secrets', 'crypto', 'runtime']);
const requires_files = 'Requires app files';

type FixedPermissionRow = {
  kind: 'fixed';
  key: string;
  label: string;
  status: string;
};

type ManagedPermissionRow = {
  kind: 'managed';
  key: ManagedPermission;
  label: string;
  unavailable_reason?: string;
};
type PickerPermissionRow = {
  kind: 'picker';
  key: 'photos';
  label: string;
  status: string;
};

function managedPermissionRow(key: ManagedPermission, has_files: boolean): ManagedPermissionRow {
  return {
    kind: 'managed',
    key,
    label: managed_permission_labels[key],
    unavailable_reason: key === 'camera' && !has_files ? requires_files : undefined,
  };
}

export function managedPermissionDeclaration(
  manifest: AppletPermissionManifest | null | undefined,
  permission: ManagedPermission,
): ManagedPermissionRow | undefined {
  const group = permission === 'microphone' ? 'audio' : permission;
  if (manifest?.permissions.includes(group)) {
    return managedPermissionRow(permission, manifest.permissions.includes('fs'));
  }
}

export function groupDevicePermissions(applets: readonly AppletPermission[]) {
  return (Object.keys(managed_permission_labels) as ManagedPermission[]).map((permission) => {
    const entries: { applet: AppletPermission; grant: PermissionGrant }[] = [];
    let allowed_count = 0;
    for (const applet of applets) {
      const grant = applet.grants.find((item) => item.permission === permission);
      if (!grant) continue;
      entries.push({ applet, grant });
      if (
        !applet.installed_revision ||
        !grant.available ||
        grant.access !== 'remembered' ||
        grant.mode !== 'remembered'
      ) {
        continue;
      }
      const declaration = managedPermissionDeclaration(applet.manifest, permission);
      if (declaration && !declaration.unavailable_reason) allowed_count++;
    }
    return { permission, entries, allowed_count };
  });
}

export function appletPermissionRows(manifest: AppletPermissionManifest) {
  const { network, permissions, has_lib } = manifest;
  const has_files = permissions.includes('fs');
  const rows: (FixedPermissionRow | ManagedPermissionRow | PickerPermissionRow)[] = [
    { kind: 'fixed', key: 'network', label: 'Network', status: network === false ? 'Off' : 'On' },
  ];
  for (const key of permissions) {
    if (key === 'photos') {
      rows.push({
        kind: 'picker',
        key,
        label: 'Photos',
        status: has_files ? 'Pick each time' : requires_files,
      });
      continue;
    }
    if (key === 'camera' || key === 'location') {
      rows.push(managedPermissionRow(key, has_files));
      continue;
    }
    const unavailable = !has_lib && backend_permissions.has(key);
    const enabled = unavailable ? 'Requires applet backend' : 'On';
    const status = (network === false ? offline_status[key] : undefined) ?? enabled;
    rows.push({ kind: 'fixed', key, label: permission_labels[key] ?? key, status });
  }
  if (permissions.includes('audio')) {
    rows.push(managedPermissionRow('microphone', has_files));
  }
  return rows;
}

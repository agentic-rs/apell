import type { AppletPermissionManifest, ManagedPermission, PermissionMode } from './types';

export const managed_permission_labels: Record<ManagedPermission, string> = {
  microphone: 'Microphone',
  camera: 'Camera',
  location: 'Location',
};

export const permission_mode_labels: Record<PermissionMode, string> = {
  ask: 'Ask every time',
  remembered: 'Always allow',
};

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
  photos: 'Photos',
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

export function appletPermissionRows(manifest: AppletPermissionManifest) {
  const { network, permissions, has_lib } = manifest;
  const has_files = permissions.includes('fs');
  const rows: (FixedPermissionRow | ManagedPermissionRow)[] = [
    { kind: 'fixed', key: 'network', label: 'Network', status: network === false ? 'Off' : 'On' },
  ];
  for (const key of permissions) {
    if (key === 'camera' || key === 'location') {
      rows.push({
        kind: 'managed',
        key,
        label: managed_permission_labels[key],
        unavailable_reason: key === 'camera' && !has_files ? requires_files : undefined,
      });
      continue;
    }
    const unavailable = !has_lib && backend_permissions.has(key);
    const enabled = unavailable ? 'Requires applet backend' : 'On';
    const status =
      key === 'photos'
        ? has_files
          ? 'Pick each time'
          : requires_files
        : ((network === false ? offline_status[key] : undefined) ?? enabled);
    rows.push({ kind: 'fixed', key, label: permission_labels[key] ?? key, status });
  }
  if (permissions.includes('audio')) {
    rows.push({ kind: 'managed', key: 'microphone', label: managed_permission_labels.microphone });
  }
  return rows;
}

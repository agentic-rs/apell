import type { StoreApplet, StoreRelease } from '../../packages/store-catalog';
export type { StoreApplet, StoreRelease } from '../../packages/store-catalog';

export interface Manifest {
  schema_version: number;
  api_version: number;
  applet_id: string;
  name: string;
  version: string;
  permissions: string[];
  network: boolean;
  lib?: string;
}
export interface InstalledApplet extends Manifest {
  sha256: string;
  icon_data_url?: string | null;
  source?: PackageSource | null;
}
export interface StoreSource {
  store_url: string;
  applet_id: string;
  channel: StoreChannel;
}
export type PackageSource =
  | {
      kind: 'github';
      repository_id: number;
      repository: string;
      asset_name: string;
      release_tag?: string | null;
      prerelease?: boolean;
    }
  | ({ kind: 'store' } & StoreSource);
export interface UnavailableApplet {
  applet_id: string;
  error: string;
}
export interface AppletListReport {
  applets: InstalledApplet[];
  unavailable: UnavailableApplet[];
}
export interface AppletPermission {
  applet_id: string;
  name: string;
  grants: PermissionGrant[];
}
export type ManagedPermission = 'microphone' | 'camera' | 'location';
export type PermissionMode = 'ask' | 'remembered';
export interface PermissionGrant {
  permission: ManagedPermission;
  access: PermissionMode | 'previous_package';
}
export interface AppletSession {
  session_id: string;
  manifest: Manifest;
  view_id: string;
  entry_url: string;
  has_lib: boolean;
}
export interface AppletInstallation {
  store_source: StoreSource | null;
  installed_version: string;
  restart_required: boolean;
}
export interface GitHubRelease {
  repository_id: number;
  repository: string;
  release_id: number;
  tag: string;
  release_tag: string | null;
  prerelease: boolean;
  assets: { asset_id: number; name: string; size_bytes: number; sha256: string | null }[];
}
export interface StoreConfig {
  store_url: string | null;
}
export type StoreChannel = 'stable' | 'nightly';
export interface StoreCatalogEntry extends StoreApplet {
  installed: InstalledApplet | null;
  update_available: boolean;
  source_conflict: boolean;
}
export interface StoreCatalog {
  store_url: string;
  applets: StoreCatalogEntry[];
}
export type StoreReleaseAction =
  'install' | 'update' | 'switch_source' | 'switch_channel' | 'installed' | 'older' | 'conflict';
export interface StoreReleaseOption extends StoreRelease {
  action: StoreReleaseAction;
  action_reason?: string;
  downloaded_bytes?: number;
}
export interface StoreSwitchSnapshot {
  sha256: string;
  source: PackageSource | null;
  removed: boolean;
}
export interface StoreDetail {
  store_url: string;
  applet: StoreApplet;
  releases: StoreReleaseOption[];
  installed: InstalledApplet | null;
  restart_required: boolean;
  installed_channel?: StoreChannel;
  switch_snapshot?: StoreSwitchSnapshot;
  channel_snapshot?: StoreSwitchSnapshot;
}
export interface StoreInstallRequest {
  store_url: string;
  applet_id: string;
  release: StoreRelease;
  expected_installed_sha256: string | null;
  switch_snapshot?: StoreSwitchSnapshot;
  channel_snapshot?: StoreSwitchSnapshot;
  installed_source?: boolean;
  operation_id: string;
}
export type StoreInstallResult =
  | { status: 'installed'; applet: InstalledApplet; restart_required: boolean }
  | { status: 'paused'; received_bytes: number };
export interface BackendEvent {
  session_id: string;
  event: { name: string; payload: unknown };
}
export type CredentialStatus = 'present' | 'absent' | 'unavailable';
export type ProviderAuthMethod = 'api_key' | 'device_code' | 'none';
export interface LlmProvider {
  provider_id: string;
  name: string;
  base_url: string;
  auth_methods: ProviderAuthMethod[];
}
export interface LlmConnection {
  connection_id: string;
  name: string;
  provider_id: string;
  base_url: string;
  model_id: string;
  credential_status: CredentialStatus;
}
export interface LlmSettings {
  active_connection_id: string | null;
  connections: LlmConnection[];
  providers: LlmProvider[];
}
export interface AccountDeviceCode {
  user_code: string;
  verification_uri: string;
  expires_at: string;
}
export interface AccountStatus {
  state: 'unavailable' | 'signed_out' | 'signing_in' | 'signed_in';
  email: string | null;
  error?: string | null;
  device_code: AccountDeviceCode | null;
}
export type KeySyncStatus =
  | { state: 'signed_out' | 'unavailable' }
  | { state: 'paired'; sync_error: string | null }
  | { state: 'unpaired'; action: 'setup' | 'pair' | 'upgrade' | 'legacy' };
export interface KeySyncPairing {
  pair_id: string;
  code: string;
  expires_at: number;
  approved: boolean;
  approval_nonce: string;
}
export interface KeySyncRecovery {
  expires_at: number;
}
export interface ConfigSyncStatus {
  state: 'disabled' | 'off' | 'ready';
  last_synced_at: string | null;
}
export interface ConfigSyncSummary {
  connections: { name: string; provider_id: string; model_id: string }[];
  active_model: string | null;
}
export interface ConfigSyncResolution {
  resolution: 'keep_local' | 'use_cloud';
  conflict_id: string;
}
export type ConfigSyncResult =
  | { state: 'current' | 'uploaded' | 'downloaded'; status: ConfigSyncStatus }
  | {
      state: 'conflict';
      status: ConfigSyncStatus;
      conflict_id: string;
      local: ConfigSyncSummary;
      cloud: ConfigSyncSummary;
    };
export interface SettingsTransferCapabilities {
  qr: boolean;
  capture_protection: 'block' | 'hide' | 'unavailable';
}
export interface SettingsTransferExport {
  payload: string;
  qr_available: boolean;
}
export interface SettingsTransferInput {
  payload: string;
  passphrase: string;
}
export interface SettingsTransferConnection {
  name: string;
  provider_id: string;
  base_url: string;
  model_id: string;
  credential: 'api_key' | 'sign_in' | 'none';
  duplicate: boolean;
}
export interface SettingsTransferPreview {
  connections: SettingsTransferConnection[];
}
export interface SettingsTransferResult {
  added: number;
  skipped: number;
}
export interface CreateConnectionDraft {
  connection_id?: string;
  provider_id: string;
  base_url: string;
}
export interface NativeConnectionDraft {
  draft_id: string;
  credential_status: CredentialStatus;
}
export interface DraftCredential {
  key_action: 'keep' | 'replace' | 'remove';
  api_key?: string;
}
export interface SaveConnection extends DraftCredential {
  draft_id: string;
  name: string;
  model_id: string;
}
export interface LlmModel {
  model_id: string;
  name: string;
}
export interface LlmModels {
  models: LlmModel[];
  source: 'provider';
}
export interface LoginChallenge {
  login_id: string;
  user_code: string;
  verification_uri: string;
  expires_in: number;
  interval: number;
}
export interface AppletEvent {
  session_id: string;
  event: {
    request_id: string;
    kind: string;
    text?: string;
    received_bytes?: number;
    total_bytes?: number;
  };
}
export interface BridgeRequest {
  type: 'request';
  request_id: string;
  method: string;
  params: unknown;
}
export interface InstallProgress {
  received_bytes: number;
  total_bytes?: number;
  operation_id?: string;
}

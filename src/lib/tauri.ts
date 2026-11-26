import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { onBackButtonPress } from '@tauri-apps/api/app';
import type { LlmResult } from '../../packages/applet-sdk/src/types';
import type {
  AccountStatus,
  AppletEvent,
  AppletInfo,
  AppletInstallResult,
  AppletInstallation,
  AppletListReport,
  AppletPermission,
  AppletSession,
  BackendEvent,
  ConfigSyncResolution,
  ConfigSyncResult,
  ConfigSyncReviewResult,
  ConfigSyncStatus,
  GitHubRelease,
  CreateConnectionDraft,
  DraftCredential,
  InstallProgress,
  KeySyncConflict,
  KeySyncConflictResolution,
  KeySyncPairing,
  KeySyncRecovery,
  KeySyncStatus,
  LocalKeySyncStatus,
  LlmConnection,
  LlmModels,
  LlmSettings,
  LoginChallenge,
  ManagedPermission,
  NativeConnectionDraft,
  PermissionMode,
  PermissionStatus,
  SaveConnection,
  SettingsTransferCapabilities,
  SettingsTransferExport,
  SettingsTransferInput,
  SettingsTransferPreview,
  SettingsTransferResult,
  StoreConfig,
  StoreCatalog,
  StoreDetail,
  StoreDownloadIdentity,
  StoreInstallRequest,
  StoreInstallResult,
  StorePendingDownload,
  StoreSavedDetail,
  StoreSwitchSnapshot,
  StoreUpdatesReport,
} from './types';

declare global {
  interface Window {
    readonly __APELL_HOST_KEY__?: string;
    readonly __APELL_PLATFORM__?: string;
  }
}

export const nativeAvailable = isTauri;
function hostKey(): string {
  if (!window.__APELL_HOST_KEY__)
    throw new Error('Open Apell in its native shell to use host services.');
  return window.__APELL_HOST_KEY__;
}
export function shellCall<T>(method: string, params: unknown = {}): Promise<T> {
  return invoke<T>('shell_command', { host_key: hostKey(), method, params });
}

export async function onNativeBack(handle: (platform: 'ios' | 'android') => void) {
  if (!nativeAvailable() || !window.__APELL_HOST_KEY__) return;
  if (window.__APELL_PLATFORM__ === 'android') {
    const listener = await onBackButtonPress(() => handle('android'));
    return () => {
      void listener.unregister().catch(() => {});
    };
  }
  if (window.__APELL_PLATFORM__ === 'ios') return listen('apell-back', () => handle('ios'));
}

export const backgroundShell = () => shellCall<void>('navigation.background');
export const listApplets = () => shellCall<AppletListReport>('applets.list');
export const getAppletInfo = (applet_id: string) =>
  shellCall<AppletInfo>('applets.info', { applet_id });
export const getStoreConfig = () => shellCall<StoreConfig>('store.config');
export const setStoreUrl = (store_url: string) =>
  shellCall<StoreConfig>('store.set_url', { store_url });
export const getStoreCatalog = (store_url: string) =>
  shellCall<StoreCatalog>('store.catalog', { store_url });
export const getSavedStoreCatalog = (store_url: string) =>
  shellCall<StoreCatalog | null>('store.saved_catalog', { store_url });
export const getStoreUpdates = () => shellCall<StoreUpdatesReport>('store.updates');
export const getSavedStoreUpdates = () => shellCall<StoreUpdatesReport>('store.saved_updates');
export const getStorePendingDownloads = () =>
  shellCall<StorePendingDownload[]>('store.pending_downloads');
export const onStoreDownloadsChanged = (handler: () => void) =>
  listen('apell-store-download-changed', handler);
const storeDownloadIdentity = ({
  origin,
  applet_id,
  version,
  channel,
  sha256,
}: StoreDownloadIdentity): StoreDownloadIdentity => ({
  origin,
  applet_id,
  version,
  channel,
  sha256,
});
export const discardStoreDownload = (download: StoreDownloadIdentity) =>
  shellCall<boolean>('store.discard_download', storeDownloadIdentity(download));
export const getStoreDetail = (store_url: string, applet_id: string, installed_source = false) =>
  shellCall<StoreDetail>('store.detail', {
    store_url,
    applet_id,
    ...(installed_source ? { installed_source } : {}),
  });
export const getSavedStoreDetail = (
  store_url: string,
  applet_id: string,
  installed_source = false,
) =>
  shellCall<StoreSavedDetail | null>('store.saved_detail', {
    store_url,
    applet_id,
    ...(installed_source ? { installed_source } : {}),
  });
export const getStoreDownloadDetail = (download: StoreDownloadIdentity) =>
  shellCall<StoreDetail>('store.detail', {
    store_url: download.origin,
    applet_id: download.applet_id,
    pending_download: storeDownloadIdentity(download),
  });
export const getStoreIcon = (store_url: string, icon_url: string) =>
  shellCall<string>('store.icon', { store_url, icon_url });
export const installStoreRelease = (request: StoreInstallRequest) =>
  shellCall<StoreInstallResult>('store.install', request);
export const installBundle = (
  params:
    | { bundle_base64: string }
    | {
        repository: string;
        release_tag: string | null;
        asset_name: string;
        release_id: number;
        asset_id: number;
      },
) => shellCall<AppletInstallResult>('applets.install', params);
export const githubRelease = (repository: string) =>
  shellCall<GitHubRelease>('applets.github_release', { repository });
export const updateApplet = (applet_id: string) =>
  shellCall<AppletInstallResult>('applets.update', { applet_id });
export const removeApplet = (applet_id: string) => shellCall<void>('applets.remove', { applet_id });
export const restoreAppletPackage = (
  applet_id: string,
  expected: StoreSwitchSnapshot,
  bundle_base64: string,
) =>
  shellCall<AppletInstallResult>('applets.restore_package', { applet_id, expected, bundle_base64 });
export const restoreStorePackage = (
  applet_id: string,
  expected: StoreSwitchSnapshot,
  operation_id: string,
) => shellCall<AppletInstallResult>('store.restore_package', { applet_id, expected, operation_id });
export const launchApplet = (applet_id: string) =>
  shellCall<AppletSession>('applets.launch', { applet_id });
export const restartApplet = (applet_id: string) =>
  shellCall<AppletSession>('applets.restart', { applet_id });
export const closeApplet = (session_id: string, view_id: string) =>
  shellCall<void>('applets.close', { session_id, view_id });
export const reloadApplet = (session_id: string, view_id: string) =>
  shellCall<AppletSession>('applets.reload', { session_id, view_id });
export const currentApplet = () => shellCall<AppletSession | null>('applets.current');
export const getAppletInstallation = (session_id: string, view_id: string) =>
  shellCall<AppletInstallation>('applets.installation', { session_id, view_id });
export const listAppletPermissions = () => shellCall<AppletPermission[]>('permissions.list');
export const openPermissionSystemSettings = (permission: ManagedPermission) =>
  shellCall<void>('permissions.open_system_settings', { permission });
export const onPermissionsChanged = (handler: () => void) =>
  listen('apell-permissions-changed', handler);
export const forgetPermission = (
  applet_id: string,
  permission: ManagedPermission,
  grant_revision: string,
) => shellCall<void>('permissions.forget', { applet_id, permission, grant_revision });
export const setInstalledPermissionMode = (
  applet_id: string,
  installed_revision: string,
  permission: ManagedPermission,
  mode: PermissionMode,
  grant_revision: string,
) =>
  shellCall<PermissionStatus>('permissions.set_installed_mode', {
    applet_id,
    installed_revision,
    permission,
    mode,
    grant_revision,
  });
export const getPermissionMode = (
  session_id: string,
  view_id: string,
  permission: ManagedPermission,
) =>
  shellCall<PermissionStatus>('permissions.mode', {
    session_id,
    view_id,
    permission,
  });
export const setPermissionMode = (
  session_id: string,
  view_id: string,
  permission: ManagedPermission,
  mode: PermissionMode,
) => shellCall<PermissionStatus>('permissions.set_mode', { session_id, view_id, permission, mode });
export const getSettings = () => shellCall<LlmSettings>('llm.settings');
export const getAccountStatus = () => shellCall<AccountStatus>('account.status');
export const signInToAccount = () => shellCall<AccountStatus>('account.sign_in');
export const openAccountSignIn = () => shellCall<void>('account.open_sign_in');
export const cancelAccountSignIn = () => shellCall<AccountStatus>('account.cancel');
const accountCall = <T>(method: string, account_session_id: string, params: object = {}) =>
  shellCall<T>(method, { ...params, account_session_id });
export const signOutOfAccount = (account_session_id: string) =>
  accountCall<AccountStatus>('account.sign_out', account_session_id);
export const onAccountChanged = (handler: () => void) => listen('apell-account-changed', handler);
export const getConfigSyncStatus = (account_session_id: string) =>
  accountCall<ConfigSyncStatus>('sync.status', account_session_id);
export const onConfigSyncChanged = (handler: () => void) =>
  listen('apell-config-sync-changed', handler);
export const setConfigSyncEnabled = (account_session_id: string, enabled: boolean) =>
  accountCall<ConfigSyncStatus>('sync.set_enabled', account_session_id, { enabled });
export const syncConfig = (account_session_id: string, resolution?: ConfigSyncResolution) =>
  accountCall<ConfigSyncResult>('sync.run', account_session_id, resolution);
export const reviewConfigSync = (account_session_id: string) =>
  accountCall<ConfigSyncReviewResult>('sync.review', account_session_id);
export const getKeySyncStatus = (account_session_id: string) =>
  accountCall<KeySyncStatus>('key_sync.status', account_session_id);
export const getLocalKeySyncStatus = (account_session_id: string) =>
  accountCall<LocalKeySyncStatus>('key_sync.local_status', account_session_id);
export const retryKeySync = (account_session_id: string) =>
  accountCall<void>('key_sync.retry', account_session_id);
export const reviewKeySyncConflicts = (account_session_id: string) =>
  accountCall<KeySyncConflict[]>('key_sync.conflicts', account_session_id);
export const resolveKeySyncConflict = (
  account_session_id: string,
  resolution: KeySyncConflictResolution,
) =>
  accountCall<{ warning: string | null }>(
    'key_sync.resolve_conflict',
    account_session_id,
    resolution,
  );
export const onKeySyncChanged = (handler: () => void) => listen('apell-key-sync-changed', handler);
export const onLlmSettingsChanged = (handler: () => void) =>
  listen('apell-llm-settings-changed', handler);
export const createKeySync = (account_session_id: string) =>
  accountCall<void>('key_sync.create', account_session_id);
export const startKeySyncPairing = (account_session_id: string) =>
  accountCall<KeySyncPairing>('key_sync.pair_start', account_session_id);
export const listKeySyncPairings = (account_session_id: string) =>
  accountCall<{ pairs: KeySyncPairing[] }>('key_sync.pair_list', account_session_id);
export const finishKeySyncPairing = (account_session_id: string, pair_id: string) =>
  accountCall<boolean>('key_sync.pair_finish', account_session_id, { pair_id });
export const cancelKeySyncPairing = (account_session_id: string, pair_id: string) =>
  accountCall<void>('key_sync.pair_cancel', account_session_id, { pair_id });
export const approveKeySyncPairing = (
  account_session_id: string,
  pair_id: string,
  approval_nonce: string,
) => accountCall<void>('key_sync.pair_approve', account_session_id, { pair_id, approval_nonce });
export const startKeySyncRecovery = (account_session_id: string) =>
  accountCall<KeySyncRecovery>('key_sync.recovery_start', account_session_id);
export const openKeySyncRecovery = (account_session_id: string, request_id: string) =>
  accountCall<void>('key_sync.recovery_open', account_session_id, { request_id });
export const finishKeySyncRecovery = (account_session_id: string, request_id: string) =>
  accountCall<boolean | 'rejected'>('key_sync.recovery_finish', account_session_id, { request_id });
export const cancelKeySyncRecovery = (account_session_id: string, request_id: string) =>
  accountCall<void>('key_sync.recovery_cancel', account_session_id, { request_id });
export const getSettingsTransferCapabilities = () =>
  shellCall<SettingsTransferCapabilities>('settings_transfer.capabilities');
export const exportSettingsTransfer = (connection_ids: string[], passphrase: string) =>
  shellCall<SettingsTransferExport>('settings_transfer.export', { connection_ids, passphrase });
export const previewSettingsTransfer = (input: SettingsTransferInput) =>
  shellCall<SettingsTransferPreview>('settings_transfer.preview', input);
export const applySettingsTransfer = (input: SettingsTransferInput) =>
  shellCall<SettingsTransferResult>('settings_transfer.apply', input);
export const saveSettingsTransferFile = (payload: string) =>
  shellCall<boolean>('settings_transfer.save_file', { payload });
export const showSettingsTransferQr = (payload: string) =>
  shellCall<void>('settings_transfer.show_qr', { payload });
export const scanSettingsTransferQr = () => shellCall<string | null>('settings_transfer.scan_qr');
export const createConnectionDraft = (params: CreateConnectionDraft) =>
  shellCall<NativeConnectionDraft>('llm.create_draft', params);
export const discardConnectionDraft = (draft_id: string) =>
  shellCall<void>('llm.discard_draft', { draft_id });
export const saveConnection = (params: SaveConnection) =>
  shellCall<LlmConnection>('llm.save_connection', params);
export const activateConnection = (connection_id: string) =>
  shellCall<void>('llm.activate_connection', { connection_id });
export const deleteConnection = (connection_id: string) =>
  shellCall<void>('llm.delete_connection', { connection_id });
export const discoverDraftModels = (draft_id: string, credential: DraftCredential) =>
  shellCall<LlmModels>('llm.discover_draft_models', { draft_id, ...credential });
export const testConnection = (connection_id: string, request_id: string) =>
  shellCall<LlmResult>('llm.test_connection', { connection_id, request_id });
export const cancelConnectionTest = (request_id: string) =>
  shellCall<void>('llm.cancel_test', { request_id });
export const beginLogin = (draft_id: string) =>
  shellCall<LoginChallenge>('llm.begin_login', { draft_id });
export const pollLogin = (login_id: string) =>
  shellCall<NativeConnectionDraft>('llm.poll_login', { login_id });
export const cancelLogin = (login_id: string) => shellCall<void>('llm.cancel_login', { login_id });
export const openLogin = (login_id: string) => shellCall<void>('llm.open_login', { login_id });
export const callApplet = (session_id: string, view_id: string, method: string, params: unknown) =>
  invoke('applet_call', { host_key: hostKey(), session_id, view_id, method, params });
export const onBackendEvent = (handler: (event: BackendEvent) => void) =>
  listen<BackendEvent>('backend-event', (event) => handler(event.payload));
export const onAppletEvent = (handler: (event: AppletEvent) => void) =>
  listen<AppletEvent>('applet-event', (event) => handler(event.payload));
export const onInstallProgress = (handler: (event: InstallProgress) => void) =>
  listen<InstallProgress>('install-progress', (event) => handler(event.payload));
export const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { LlmResult } from '../../packages/applet-sdk/src/types';
import type {
  AppletEvent,
  AppletListReport,
  AppletPermission,
  AppletSession,
  BackendEvent,
  GitHubRelease,
  CreateConnectionDraft,
  DraftCredential,
  InstalledApplet,
  InstallProgress,
  LlmConnection,
  LlmModels,
  LlmSettings,
  LoginChallenge,
  NativeConnectionDraft,
  SaveConnection,
} from './types';

declare global {
  interface Window {
    readonly __APELL_HOST_KEY__?: string;
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
export const listApplets = () => shellCall<AppletListReport>('applets.list');
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
) => shellCall<InstalledApplet>('applets.install', params);
export const githubRelease = (repository: string) =>
  shellCall<GitHubRelease>('applets.github_release', { repository });
export const updateApplet = (applet_id: string) =>
  shellCall<InstalledApplet>('applets.update', { applet_id });
export const launchApplet = (applet_id: string) =>
  shellCall<AppletSession>('applets.launch', { applet_id });
export const closeApplet = (session_id: string, view_id: string) =>
  shellCall<void>('applets.close', { session_id, view_id });
export const reloadApplet = (session_id: string, view_id: string) =>
  shellCall<AppletSession>('applets.reload', { session_id, view_id });
export const currentApplet = () => shellCall<AppletSession | null>('applets.current');
export const listAppletPermissions = () => shellCall<AppletPermission[]>('permissions.list');
export const forgetMicrophone = (applet_id: string) =>
  shellCall<void>('permissions.forget_microphone', { applet_id });
export const getSettings = () => shellCall<LlmSettings>('llm.settings');
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

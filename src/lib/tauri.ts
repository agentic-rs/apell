import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type {
  AppletEvent,
  AppletSession,
  InstalledApplet,
  InstallProgress,
  LlmSettings,
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
export const listApplets = () => shellCall<InstalledApplet[]>('applets.list');
export const installBundle = (
  params:
    | { bundle: string; sha256?: string; url?: never }
    | { url: string; sha256: string; bundle?: never },
) => shellCall<InstalledApplet>('applets.install', params);
export const launchApplet = (applet_id: string) =>
  shellCall<AppletSession>('applets.launch', { applet_id });
export const closeApplet = (session_id: string) => shellCall<void>('applets.close', { session_id });
export const getSettings = () => shellCall<LlmSettings>('llm.settings');
export const saveSettings = (
  config: Pick<LlmSettings, 'base_url' | 'model_id'>,
  api_key: string | null,
) => shellCall<void>('llm.save_settings', { config, api_key });
export const callApplet = (session_id: string, method: string, params: unknown) =>
  invoke('applet_call', { host_key: hostKey(), session_id, method, params });
export const onAppletEvent = (handler: (event: AppletEvent) => void) =>
  listen<AppletEvent>('applet-event', (event) => handler(event.payload));
export const onInstallProgress = (handler: (event: InstallProgress) => void) =>
  listen<InstallProgress>('install-progress', (event) => handler(event.payload));
export const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

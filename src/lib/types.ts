export interface Manifest {
  schema_version: number;
  api_version: number;
  applet_id: string;
  name: string;
  version: string;
  permissions: string[];
  lib?: string;
}
export interface InstalledApplet extends Manifest {
  sha256: string;
  source?: {
    repository_id: number;
    repository: string;
    asset_name: string;
    release_tag?: string | null;
    prerelease?: boolean;
  } | null;
}
export interface UnavailableApplet {
  applet_id: string;
  error: string;
}
export interface AppletListReport {
  applets: InstalledApplet[];
  unavailable: UnavailableApplet[];
}
export interface AppletSession {
  session_id: string;
  manifest: Manifest;
  view_id: string;
  entry_url: string;
  has_lib: boolean;
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
  active_connection_id: string;
  connections: LlmConnection[];
  providers: LlmProvider[];
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
  source: 'provider' | 'catalogue';
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
}

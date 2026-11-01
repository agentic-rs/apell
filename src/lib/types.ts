export interface Manifest {
  schema_version: number;
  api_version: number;
  applet_id: string;
  name: string;
  version: string;
  permissions: string[];
}
export interface InstalledApplet extends Manifest {
  sha256: string;
}
export interface AppletSession {
  session_id: string;
  manifest: Manifest;
  html: string;
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
export interface SaveConnection {
  connection_id?: string;
  name: string;
  provider_id: string;
  base_url: string;
  model_id: string;
  key_action: 'keep' | 'replace' | 'remove';
  api_key?: string;
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

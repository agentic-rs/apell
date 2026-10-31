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
export interface LlmSettings {
  base_url: string;
  model_id: string;
  connection_id: string;
  has_api_key: boolean;
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

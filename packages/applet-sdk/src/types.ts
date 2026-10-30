export type SqlValue =
  | { type: 'null' }
  | { type: 'integer'; value: string }
  | { type: 'real'; value: number }
  | { type: 'text'; value: string }
  | { type: 'blob'; value: number[] };

export interface QueryResult {
  columns: string[];
  rows: SqlValue[][];
}

export interface LlmEvent {
  request_id: string;
  kind: 'delta' | 'done' | 'error';
  text: string;
}

export interface DownloadProgress {
  request_id: string;
  kind: 'download_progress';
  received_bytes: number;
  total_bytes: number | null;
}

export interface LlmConfiguration {
  model_id: string;
  mode: 'demo' | 'provider';
}

export interface LlmMessage {
  role: 'system' | 'user' | 'assistant';
  content: string;
}

export interface LlmRequest {
  request_id: string;
  messages: LlmMessage[];
  temperature?: number;
}

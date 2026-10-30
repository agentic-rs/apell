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

export type JsonValue =
  null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };
export type ConfigEntry = { found: false } | { found: true; value: JsonValue };

export interface HttpHeader {
  name: string;
  /** HTTP byte string: each character U+0000–U+00FF represents one byte. */
  value: string;
}
export interface HttpRequest {
  url: string;
  method?: 'GET' | 'HEAD' | 'POST' | 'PUT' | 'PATCH' | 'DELETE' | 'OPTIONS';
  headers?: HttpHeader[];
  body?: number[];
  timeout_ms?: number;
}
export interface HttpResponse {
  status: number;
  headers: HttpHeader[];
  body: number[];
}
export interface FileReadOptions {
  offset?: number;
  length?: number;
}
export interface FileReadResult {
  bytes: number[];
  eof: boolean;
}
export interface FileStat {
  kind: 'file' | 'directory';
  size_bytes: number;
}
export interface DirectoryEntry extends FileStat {
  name: string;
}

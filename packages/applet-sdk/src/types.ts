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
  kind: 'started' | 'delta' | 'reasoning_delta' | 'tool_call' | 'terminal' | 'done' | 'error';
  text: string;
  result?: LlmResult;
  tool_call?: LlmToolCall;
  provenance?: LlmProvenance;
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
  connection_id: string;
  capabilities: LlmCapabilities;
}

export type LlmCapability = 'supported' | 'unsupported' | 'unknown';

export interface LlmCapabilities {
  tools: LlmCapability;
  required_tool_choice: LlmCapability;
  exposed_reasoning: LlmCapability;
}

export interface LlmToolCall {
  id: string;
  name: string;
  arguments: string;
}

export interface LlmTool {
  name: string;
  description?: string;
  parameters: JsonValue;
}

export type LlmToolChoice = 'auto' | 'none' | 'required' | { name: string };

export type LlmMessage =
  | { role: 'system' | 'user'; content: string }
  | {
      role: 'assistant';
      content: string;
      tool_calls?: LlmToolCall[];
      /** Opaque, transient host metadata; return unchanged during tool continuation. */
      continuation_id?: string | null;
    }
  | { role: 'tool'; content: string; tool_call_id: string };

export interface LlmRequest {
  request_id: string;
  messages: LlmMessage[];
  temperature?: number;
  tools?: LlmTool[];
  tool_choice?: LlmToolChoice;
}

export interface LlmProvenance {
  connection_id: string;
  requested_model_id: string;
  /** The provider-reported model, or null when it was not reported. */
  model_id: string | null;
}

export type LlmStatus =
  'completed' | 'tool_calls' | 'cancelled' | 'truncated' | 'unsupported' | 'failed';

export interface LlmFailure {
  code: string;
  message: string;
  capability?: string | null;
  retryable: boolean;
}

export interface LlmResult {
  request_id: string;
  content: string;
  tool_calls: LlmToolCall[];
  continuation_id: string | null;
  status: LlmStatus;
  finish_reason: string | null;
  provenance: LlmProvenance;
  error: LlmFailure | null;
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

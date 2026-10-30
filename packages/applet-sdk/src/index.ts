import type {
  ConfigEntry,
  DirectoryEntry,
  DownloadProgress,
  FileReadOptions,
  FileReadResult,
  FileStat,
  HttpRequest,
  HttpResponse,
  JsonValue,
  LlmEvent,
} from './types';
import { assertJsonValue } from './json';

export type * from './types';

type Pending = {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timeout: ReturnType<typeof setTimeout>;
  llm_request_id?: string;
};

export class HostError extends Error {
  readonly partial_content: string;

  constructor(message: string, partial_content = '') {
    super(message);
    this.name = 'HostError';
    this.partial_content = partial_content;
  }
}

const pending = new Map<string, Pending>();
const streams = new Map<string, string>();
const subscribers = new Set<(event: LlmEvent) => void>();
const downloadSubscribers = new Set<(event: DownloadProgress) => void>();
let port: MessagePort | undefined;
let resolveReady: (value: MessagePort) => void;
const ready = new Promise<MessagePort>((resolve) => {
  resolveReady = resolve;
});

function handleMessage(event: MessageEvent<unknown>): void {
  const message = event.data;
  if (!message || typeof message !== 'object') return;
  const envelope = message as Record<string, unknown>;
  if (envelope.type === 'event') {
    if (envelope.event && typeof envelope.event === 'object') {
      const download = envelope.event as DownloadProgress;
      if (
        download.kind === 'download_progress' &&
        typeof download.request_id === 'string' &&
        Number.isSafeInteger(download.received_bytes) &&
        download.received_bytes >= 0 &&
        (download.total_bytes === null ||
          (Number.isSafeInteger(download.total_bytes) && download.total_bytes >= 0))
      ) {
        for (const subscriber of downloadSubscribers) subscriber(download);
        return;
      }
    }
    const value = envelope.event as LlmEvent | undefined;
    if (
      !value ||
      typeof value.request_id !== 'string' ||
      typeof value.text !== 'string' ||
      !['delta', 'done', 'error'].includes(value.kind)
    )
      return;
    if (value.kind === 'delta' && streams.has(value.request_id)) {
      streams.set(value.request_id, (streams.get(value.request_id) ?? '') + value.text);
    }
    for (const subscriber of subscribers) subscriber(value);
    return;
  }
  if (envelope.type !== 'response' || typeof envelope.request_id !== 'string') return;
  const request = pending.get(envelope.request_id);
  if (!request) return;
  pending.delete(envelope.request_id);
  clearTimeout(request.timeout);
  const partial = request.llm_request_id ? (streams.get(request.llm_request_id) ?? '') : '';
  if (request.llm_request_id) streams.delete(request.llm_request_id);
  if (typeof envelope.error === 'string') request.reject(new HostError(envelope.error, partial));
  else request.resolve(envelope.result);
}

window.addEventListener('message', (event: MessageEvent) => {
  if (
    port ||
    event.source !== window.parent ||
    event.data?.type !== 'apell:init' ||
    !event.ports[0]
  )
    return;
  port = event.ports[0];
  port.onmessage = handleMessage;
  port.start();
  resolveReady(port);
});
// Announce after installing the receiver. An iframe's first load event can
// belong to its initial about:blank document rather than the applet itself.
window.parent.postMessage({ type: 'apell:ready' }, '*');

export function createId(): string {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID();
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

export async function hostCall<T = unknown>(
  method: string,
  params: Record<string, unknown> = {},
): Promise<T> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  const channel = await Promise.race([
    ready,
    new Promise<never>((_, reject) => {
      timeout = setTimeout(
        () => reject(new HostError('The applet host did not connect. Reopen this applet.')),
        15_000,
      );
    }),
  ]).finally(() => {
    if (timeout) clearTimeout(timeout);
  });
  const request_id = createId();
  const llm_request_id =
    method === 'llm.complete' && typeof params.request_id === 'string'
      ? params.request_id
      : undefined;
  if (llm_request_id) streams.set(llm_request_id, '');
  return new Promise<T>((resolve, reject) => {
    const request_timeout = setTimeout(
      () => {
        pending.delete(request_id);
        const partial = llm_request_id ? (streams.get(llm_request_id) ?? '') : '';
        if (llm_request_id) {
          streams.delete(llm_request_id);
          void hostCall('llm.cancel', { request_id: llm_request_id }).catch(() => undefined);
        }
        reject(new HostError('The host request timed out.', partial));
      },
      method === 'llm.complete'
        ? 330_000
        : method === 'resources.download' || method === 'http.request'
          ? 135_000
          : 35_000,
    );
    pending.set(request_id, {
      resolve: (value) => resolve(value as T),
      reject,
      timeout: request_timeout,
      llm_request_id,
    });
    channel.postMessage({ type: 'request', request_id, method, params });
  });
}

export function onLlmEvent(handler: (event: LlmEvent) => void): () => void {
  subscribers.add(handler);
  return () => {
    subscribers.delete(handler);
  };
}

export function onDownloadProgress(handler: (event: DownloadProgress) => void): () => void {
  downloadSubscribers.add(handler);
  return () => {
    downloadSubscribers.delete(handler);
  };
}

/** Native HTTP; non-2xx statuses are returned for the applet to interpret. */
export const http = {
  request(request: HttpRequest): Promise<HttpResponse> {
    return hostCall('http.request', { ...request });
  },
};

/** Relative paths in this applet's private file directory. */
export const fs = {
  read(path: string, options: FileReadOptions = {}): Promise<FileReadResult> {
    return hostCall('fs.read', { path, ...options });
  },
  write(path: string, bytes: number[] | Uint8Array): Promise<{ size_bytes: number }> {
    return hostCall('fs.write', { path, bytes: Array.from(bytes) });
  },
  list(path = ''): Promise<{ entries: DirectoryEntry[] }> {
    return hostCall('fs.list', { path });
  },
  stat(path: string): Promise<FileStat> {
    return hostCall('fs.stat', { path });
  },
  mkdir(path: string): Promise<void> {
    return hostCall('fs.mkdir', { path });
  },
  remove(path: string): Promise<void> {
    return hostCall('fs.remove', { path });
  },
  rename(from_path: string, to_path: string): Promise<void> {
    return hostCall('fs.rename', { from_path, to_path });
  },
};

/** Non-secret JSON preferences, persisted independently of applet files. */
export const config = {
  get(key: string): Promise<ConfigEntry> {
    return hostCall('config.get', { key });
  },
  async set(key: string, value: JsonValue): Promise<void> {
    assertJsonValue(value);
    return hostCall('config.set', { key, value });
  },
  delete(key: string): Promise<{ deleted: boolean }> {
    return hostCall('config.delete', { key });
  },
};

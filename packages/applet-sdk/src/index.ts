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
  LlmConfiguration,
  LlmEvent,
  LlmGenerateOptions,
  LlmRequest,
  LlmResult,
} from './types';
import { assertJsonValue } from './json';

export type * from './types';

type Pending = {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timeout: ReturnType<typeof setTimeout>;
  llm?: ActiveLlm;
};

type ActiveLlm = {
  request_id: string;
  partial_content: string;
  on_event?: (event: LlmEvent) => void;
  signal?: AbortSignal;
  abort_handler?: () => void;
  channel?: MessagePort;
  cancel_requested: boolean;
  cancel_sent: boolean;
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
const activeLlm = new Map<string, ActiveLlm>();
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
      !['started', 'text_delta', 'reasoning_delta'].includes(value.kind)
    )
      return;
    const active = activeLlm.get(value.request_id);
    if (!active) return;
    if (value.kind === 'text_delta') {
      active.partial_content += value.text;
    }
    // UI observers cannot interrupt another request or prevent its completion.
    try {
      active.on_event?.(value);
    } catch {
      // The authoritative outcome still arrives through the request promise.
    }
    return;
  }
  if (envelope.type !== 'response' || typeof envelope.request_id !== 'string') return;
  const request = pending.get(envelope.request_id);
  if (!request) return;
  pending.delete(envelope.request_id);
  clearTimeout(request.timeout);
  const partial = request.llm?.partial_content ?? '';
  releaseLlm(request.llm);
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
  return callHost(method, params);
}

function releaseLlm(active: ActiveLlm | undefined): void {
  if (!active) return;
  if (activeLlm.get(active.request_id) === active) activeLlm.delete(active.request_id);
  if (active.signal && active.abort_handler) {
    active.signal.removeEventListener('abort', active.abort_handler);
  }
  active.on_event = undefined;
  active.channel = undefined;
}

function notifyCancellation(active: ActiveLlm): void {
  if (!active.channel || active.cancel_sent) return;
  active.cancel_sent = true;
  try {
    // The cancellation acknowledgement is unnecessary. Do not retain a second
    // pending promise or timer after the generation has already settled.
    active.channel.postMessage({
      type: 'request',
      request_id: createId(),
      method: 'llm.cancel',
      params: { request_id: active.request_id },
    });
  } catch {
    // Cancellation is best effort when the bridge itself has failed. The
    // generation still resolves from the host or rejects with its partial text.
  }
}

function reserveLlm(request_id: string, options: LlmGenerateOptions): ActiveLlm {
  if (activeLlm.has(request_id)) throw new HostError('This LLM request is already active.');
  const active: ActiveLlm = {
    request_id,
    partial_content: '',
    on_event: options.on_event,
    signal: options.signal,
    cancel_requested: false,
    cancel_sent: false,
  };
  activeLlm.set(request_id, active);
  if (options.signal) {
    active.abort_handler = () => {
      active.cancel_requested = true;
      notifyCancellation(active);
    };
    options.signal.addEventListener('abort', active.abort_handler, { once: true });
    if (options.signal.aborted) active.abort_handler();
  }
  return active;
}

async function connectHost(): Promise<MessagePort> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  return Promise.race([
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
}

async function callHost<T>(
  method: string,
  params: Record<string, unknown>,
  options: LlmGenerateOptions = {},
): Promise<T> {
  const is_llm_request = method === 'llm.generate';
  const active =
    is_llm_request && typeof params.request_id === 'string'
      ? reserveLlm(params.request_id, options)
      : undefined;
  try {
    const channel = await connectHost();
    if (active) {
      active.channel = channel;
      // A pre-aborted signal reaches the native early-cancellation registry
      // before generation, which still returns its authoritative outcome.
      if (active.cancel_requested) notifyCancellation(active);
    }
    const request_id = createId();
    return await new Promise<T>((resolve, reject) => {
      const request_timeout = setTimeout(
        () => {
          pending.delete(request_id);
          const partial = active?.partial_content ?? '';
          if (active) notifyCancellation(active);
          releaseLlm(active);
          reject(new HostError('The host request timed out.', partial));
        },
        is_llm_request
          ? 330_000
          : method === 'resources.download' || method === 'http.request'
            ? 135_000
            : 35_000,
      );
      pending.set(request_id, {
        resolve: (value) => resolve(value as T),
        reject,
        timeout: request_timeout,
        llm: active,
      });
      try {
        channel.postMessage({ type: 'request', request_id, method, params });
      } catch (error) {
        pending.delete(request_id);
        clearTimeout(request_timeout);
        const partial = active?.partial_content ?? '';
        releaseLlm(active);
        reject(new HostError(error instanceof Error ? error.message : String(error), partial));
      }
    });
  } finally {
    releaseLlm(active);
  }
}

export function onDownloadProgress(handler: (event: DownloadProgress) => void): () => void {
  downloadSubscribers.add(handler);
  return () => {
    downloadSubscribers.delete(handler);
  };
}

/** Shared model access. Tools run only when the applet explicitly executes them. */
export const llm = {
  config(): Promise<LlmConfiguration> {
    return hostCall('llm.config');
  },
  async generate(request: LlmRequest, options: LlmGenerateOptions = {}): Promise<LlmResult> {
    const params = omitUndefined({ ...request, request_id: request.request_id ?? createId() }, [
      'temperature',
      'tools',
      'tool_choice',
      'stream',
      'max_output_tokens',
    ]);
    if (request.tools !== undefined) {
      params.tools = request.tools.map((tool) => omitUndefined({ ...tool }, ['description']));
    }
    assertJsonValue(params);
    return callHost('llm.generate', params, options);
  },
  cancel(request_id: string): Promise<void> {
    return hostCall('llm.cancel', { request_id });
  },
};

function omitUndefined(
  value: Record<string, unknown>,
  optional_fields: string[],
): Record<string, unknown> {
  for (const field of optional_fields) {
    if (value[field] === undefined) delete value[field];
  }
  return value;
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

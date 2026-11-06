import type { BridgeRequest } from './types';
import { assertJsonValue } from '../../packages/applet-sdk/src/json';

const methods = new Set([
  'backend.invoke',
  'sqlite.open',
  'sqlite.close',
  'sqlite.query',
  'sqlite.execute',
  'sqlite.begin',
  'sqlite.commit',
  'sqlite.rollback',
  'llm.config',
  'llm.choose_model',
  'llm.generate',
  'llm.cancel',
  'resources.download',
  'resources.read',
  'device.info',
  'http.request',
  'fs.read',
  'fs.write',
  'fs.list',
  'fs.stat',
  'fs.mkdir',
  'fs.remove',
  'fs.rename',
  'config.get',
  'config.set',
  'config.delete',
  'audio.capabilities',
  'audio.start',
  'audio.pause',
  'audio.resume',
  'audio.stop',
  'audio.import',
  'audio.share',
  'audio.play',
  'audio.playback_pause',
  'audio.seek',
  'audio.status',
  'speech.status',
  'speech.download_model',
  'speech.transcribe',
  'speech.cancel',
  'camera.capture',
  'photos.pick',
  'location.current',
]);
export function isBridgeRequest(value: unknown): value is BridgeRequest {
  if (!value || typeof value !== 'object') return false;
  const request = value as Partial<BridgeRequest>;
  return (
    request.type === 'request' &&
    typeof request.request_id === 'string' &&
    request.request_id.length > 0 &&
    request.request_id.length <= 100 &&
    typeof request.method === 'string' &&
    methods.has(request.method) &&
    request.params !== null &&
    typeof request.params === 'object'
  );
}

export function attachBridge(
  port: MessagePort,
  call: (method: string, params: unknown) => Promise<unknown>,
): () => void {
  let closed = false;
  const active = new Set<string>();
  port.onmessage = async ({ data }: MessageEvent<unknown>) => {
    if (!isBridgeRequest(data) || closed) return;
    if (active.has(data.request_id) || active.size >= 32) {
      port.postMessage({
        type: 'response',
        request_id: data.request_id,
        error: 'Too many requests or duplicate request ID.',
      });
      return;
    }
    active.add(data.request_id);
    try {
      if (data.method === 'config.set') {
        assertJsonValue((data.params as Record<string, unknown>).value);
      }
      if (
        data.method === 'llm.generate' ||
        data.method === 'llm.config' ||
        data.method === 'llm.choose_model' ||
        data.method === 'backend.invoke'
      ) {
        // Raw WASM callers bypass the SDK helper. Validate before native JSON
        // serialization can silently turn NaN into null or omit undefined.
        assertJsonValue(data.params);
      }
      const result = await call(data.method, data.params);
      if (!closed) port.postMessage({ type: 'response', request_id: data.request_id, result });
    } catch (error) {
      if (!closed)
        port.postMessage({
          type: 'response',
          request_id: data.request_id,
          error: error instanceof Error ? error.message : String(error),
        });
    } finally {
      active.delete(data.request_id);
    }
  };
  port.start();
  return () => {
    closed = true;
    port.onmessage = null;
    port.close();
  };
}

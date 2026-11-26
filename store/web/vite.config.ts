import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig, type ServerOptions } from 'vite';

const root = fileURLToPath(new URL('.', import.meta.url));

function localStoreServer(): ServerOptions {
  const backend = new URL(process.env.APELL_DEV_STORE_BACKEND ?? 'http://127.0.0.1:8789');
  if (
    backend.protocol !== 'http:' ||
    !['127.0.0.1', 'localhost', '[::1]'].includes(backend.hostname) ||
    backend.href !== `${backend.origin}/`
  ) {
    throw new Error('APELL_DEV_STORE_BACKEND must be a loopback HTTP origin.');
  }

  const tls_dir = process.env.APELL_DEV_TLS_DIR;
  const proxy = { target: backend.origin, changeOrigin: false };
  return {
    host: '127.0.0.1',
    port: 8788,
    strictPort: true,
    fs: { allow: [root, fileURLToPath(new URL('../../packages', import.meta.url))] },
    https: tls_dir
      ? {
          cert: readFileSync(join(tls_dir, 'localhost.crt')),
          key: readFileSync(join(tls_dir, 'localhost.key')),
        }
      : undefined,
    proxy: {
      '^/api(?:/|\\?|$)': proxy,
      '^/media(?:/|\\?|$)': proxy,
      '^/healthz(?:\\?|$)': proxy,
    },
  };
}

export default defineConfig(({ command, isPreview }) => ({
  root,
  build: { target: 'es2022', outDir: 'dist', emptyOutDir: true },
  server: command === 'serve' && !isPreview ? localStoreServer() : undefined,
}));

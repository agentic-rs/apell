import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';

const root = fileURLToPath(new URL('../site', import.meta.url));

export default defineConfig({
  root,
  server: {
    host: '127.0.0.1',
    port: 8877,
    strictPort: true,
    fs: { allow: [root] },
  },
});

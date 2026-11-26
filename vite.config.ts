import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    host: process.env.TAURI_DEV_HOST || '127.0.0.1',
    port: 1420,
    strictPort: true,
    fs: {
      deny: ['.env', '.env.*', '*.{crt,pem,key}', '**/.git/**', '**/.local/**', '**/.state/**'],
    },
    hmr: process.env.TAURI_DEV_HOST ? { host: process.env.TAURI_DEV_HOST, port: 1421 } : undefined,
  },
  build: { target: 'es2022' },
});

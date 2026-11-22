import { createRequire } from 'node:module';

// Tauri forwards TAURI-prefixed values to the native mobile build scripts.
if (process.argv[2] === 'ios' || process.argv[2] === 'android') {
  for (const name of [
    'APELL_ACCOUNT_ISSUER',
    'APELL_ACCOUNT_CLIENT_ID',
    'APELL_SYNC_URL',
    'APELL_SYNC_RESOURCE',
  ]) {
    const value = process.env[name];
    if (value !== undefined) process.env[`TAURI_${name}`] = value;
  }
}

// Tauri reads NDK_HOME, while the Whisper CMake build reads ANDROID_NDK_ROOT.
if (process.argv[2] === 'android' && !process.env.ANDROID_NDK_ROOT && !process.env.ANDROID_NDK) {
  if (process.env.NDK_HOME) process.env.ANDROID_NDK_ROOT = process.env.NDK_HOME;
}

createRequire(import.meta.url)('@tauri-apps/cli/tauri.js');

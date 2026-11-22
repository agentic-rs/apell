import { createRequire } from 'node:module';
import { migrateIosProjectName, syncAndroidAppName } from './mobile-project.ts';

const [, , platform, command, ...args] = process.argv;
const query = args.some((arg) => ['--help', '-h', '--version', '-V'].includes(arg));
if (!query && (command === 'build' || command === 'dev' || command === 'run')) {
  if (platform === 'ios') await migrateIosProjectName();
  if (platform === 'android') await syncAndroidAppName();
}

// Tauri forwards TAURI-prefixed values to the native mobile build scripts.
if (platform === 'ios' || platform === 'android') {
  for (const name of [
    'APELL_ACCOUNT_ISSUER',
    'APELL_ACCOUNT_CLIENT_ID',
    'APELL_SYNC_URL',
    'APELL_SYNC_RESOURCE',
    'APELL_KEY_EXCHANGE_RESOURCE',
  ]) {
    const value = process.env[name];
    if (value !== undefined) process.env[`TAURI_${name}`] = value;
  }
}

// Tauri reads NDK_HOME, while the Whisper CMake build reads ANDROID_NDK_ROOT.
if (platform === 'android' && !process.env.ANDROID_NDK_ROOT && !process.env.ANDROID_NDK) {
  if (process.env.NDK_HOME) process.env.ANDROID_NDK_ROOT = process.env.NDK_HOME;
}

createRequire(import.meta.url)('@tauri-apps/cli/tauri.js');

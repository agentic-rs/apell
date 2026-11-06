import { createRequire } from 'node:module';

// Tauri reads NDK_HOME, while the Whisper CMake build reads ANDROID_NDK_ROOT.
if (process.argv[2] === 'android' && !process.env.ANDROID_NDK_ROOT && !process.env.ANDROID_NDK) {
  if (process.env.NDK_HOME) process.env.ANDROID_NDK_ROOT = process.env.NDK_HOME;
}

createRequire(import.meta.url)('@tauri-apps/cli/tauri.js');

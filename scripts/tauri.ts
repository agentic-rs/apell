import { createRequire } from 'node:module';
import { migrateIosProjectName, syncAndroidAppName } from './mobile-project.ts';
import { loadShellAccountEnvironment, shellBuildTarget } from './shell-account.ts';

const args = process.argv.slice(2);
const [platform] = args;
const target = shellBuildTarget(args);
if (target) {
  await loadShellAccountEnvironment();
  if (target === 'ios') await migrateIosProjectName();
  if (target === 'android') await syncAndroidAppName();
}

// Tauri reads NDK_HOME, while the Whisper CMake build reads ANDROID_NDK_ROOT.
if (platform === 'android' && !process.env.ANDROID_NDK_ROOT && !process.env.ANDROID_NDK) {
  if (process.env.NDK_HOME) process.env.ANDROID_NDK_ROOT = process.env.NDK_HOME;
}

createRequire(import.meta.url)('@tauri-apps/cli/tauri.js');

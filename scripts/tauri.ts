import { createRequire } from 'node:module';
import { migrateIosProjectName, syncAndroidAppName } from './mobile-project.ts';
import { loadShellAccountEnvironment } from './shell-account.ts';

const [, , platform, command, ...args] = process.argv;
const query = args.some((arg) => ['--help', '-h', '--version', '-V'].includes(arg));
const mobile_build =
  (platform === 'ios' || platform === 'android') && ['build', 'dev', 'run'].includes(command);
if (!query && mobile_build) {
  if (platform === 'ios') await migrateIosProjectName();
  if (platform === 'android') await syncAndroidAppName();
}
if (!query && (mobile_build || platform === 'dev' || platform === 'build')) {
  await loadShellAccountEnvironment();
}

// Tauri reads NDK_HOME, while the Whisper CMake build reads ANDROID_NDK_ROOT.
if (platform === 'android' && !process.env.ANDROID_NDK_ROOT && !process.env.ANDROID_NDK) {
  if (process.env.NDK_HOME) process.env.ANDROID_NDK_ROOT = process.env.NDK_HOME;
}

createRequire(import.meta.url)('@tauri-apps/cli/tauri.js');

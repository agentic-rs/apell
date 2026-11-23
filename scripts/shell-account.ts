import { readFile } from 'node:fs/promises';
import { parseEnv } from 'node:util';

const account_variables = [
  'APELL_ACCOUNT_ISSUER',
  'APELL_ACCOUNT_CLIENT_ID',
  'APELL_SYNC_URL',
  'APELL_SYNC_RESOURCE',
  'APELL_KEY_EXCHANGE_RESOURCE',
] as const;

export function shellBuildTarget(
  args: readonly string[],
): 'desktop' | 'ios' | 'android' | undefined {
  const [platform, command] = args;
  if (args.some((arg) => ['--help', '-h', '--version', '-V'].includes(arg))) return;
  if (platform === 'build' || platform === 'dev') return 'desktop';
  if ((platform === 'ios' || platform === 'android') && ['build', 'dev', 'run'].includes(command)) {
    return platform;
  }
}

function httpsAddress(name: string, value: string): URL {
  const url = URL.parse(value);
  if (
    !url ||
    url.protocol !== 'https:' ||
    url.username ||
    url.password ||
    value.includes('?') ||
    value.includes('#') ||
    Buffer.byteLength(value) > 2048 ||
    (url.href !== value && url.origin !== value)
  ) {
    throw new Error(
      `${name} must be a canonical HTTPS URL without credentials, query, or fragment.`,
    );
  }
  return url;
}

function checkKeySyncProfile(profile: NodeJS.ProcessEnv): void {
  const {
    APELL_ACCOUNT_ISSUER: issuer,
    APELL_ACCOUNT_CLIENT_ID: client_id,
    APELL_SYNC_URL: store,
    APELL_SYNC_RESOURCE: store_resource,
    APELL_KEY_EXCHANGE_RESOURCE: relay_resource,
  } = profile;
  if (!issuer || !client_id || !store || !store_resource || !relay_resource) {
    const missing = account_variables.filter((name) => !profile[name]);
    throw new Error(`Missing shell build profile settings (${missing.join(', ')}).`);
  }
  if (!/^[A-Za-z0-9_-]{1,128}$/.test(client_id)) {
    throw new Error('APELL_ACCOUNT_CLIENT_ID must be a valid native client ID.');
  }
  const issuer_url = httpsAddress('APELL_ACCOUNT_ISSUER', issuer);
  const store_url = httpsAddress('APELL_SYNC_URL', store);
  const store_audience = httpsAddress('APELL_SYNC_RESOURCE', store_resource);
  const relay_audience = httpsAddress('APELL_KEY_EXCHANGE_RESOURCE', relay_resource);
  if (store_url.pathname !== '/') {
    throw new Error('APELL_SYNC_URL must be the Store origin.');
  }
  if (relay_audience.origin !== issuer_url.origin || relay_audience.href === store_audience.href) {
    throw new Error(
      'APELL_KEY_EXCHANGE_RESOURCE must share the Account origin and differ from APELL_SYNC_RESOURCE.',
    );
  }
}

export async function loadShellAccountEnvironment(
  environment: NodeJS.ProcessEnv = process.env,
  settings_path = new URL('../deploy/account/.env.shell-pilot', import.meta.url),
  require_key_sync = environment.APELL_REQUIRE_KEY_SYNC === '1',
): Promise<void> {
  const explicit_profile = environment.APELL_SHELL_BUILD_PROFILE;
  const settings =
    explicit_profile ??
    (await readFile(settings_path, 'utf8').catch((error: NodeJS.ErrnoException) => {
      if (error.code === 'ENOENT') return '';
      throw error;
    }));
  const parsed = parseEnv(settings);
  const profile: NodeJS.ProcessEnv = {};
  for (const name of account_variables) {
    profile[name] =
      explicit_profile === undefined
        ? (environment[name] ?? environment[`TAURI_${name}`] ?? parsed[name])
        : parsed[name];
  }
  if (require_key_sync || explicit_profile !== undefined) checkKeySyncProfile(profile);
  delete environment.APELL_SHELL_BUILD_PROFILE;
  for (const name of account_variables) {
    const value = profile[name];
    if (value === undefined) continue;
    environment[name] = value;
    // Tauri forwards only prefixed values to the native mobile build scripts.
    environment[`TAURI_${name}`] = value;
  }
}

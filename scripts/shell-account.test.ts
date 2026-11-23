import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { loadShellAccountEnvironment, shellBuildTarget } from './shell-account.ts';

const example_profile = {
  APELL_ACCOUNT_ISSUER: 'https://account.example.invalid/oidc',
  APELL_ACCOUNT_CLIENT_ID: 'shell-device',
  APELL_SYNC_URL: 'https://store.example.invalid',
  APELL_SYNC_RESOURCE: 'https://store.example.invalid/config',
  APELL_KEY_EXCHANGE_RESOURCE: 'https://account.example.invalid/key-exchange',
};

function dotenv(settings: Record<string, string>): string {
  return Object.entries(settings)
    .map(([key, value]) => `${key}="${value}"`)
    .join('\n');
}

describe('shell Account build settings', () => {
  let directory: string;
  let settings: URL;

  beforeEach(async () => {
    directory = await mkdtemp(join(tmpdir(), 'apell-account-'));
    settings = pathToFileURL(join(directory, '.env.shell-pilot'));
  });

  afterEach(async () => {
    await rm(directory, { recursive: true, force: true });
  });

  it('forwards only the public shell settings to the native build', async () => {
    await writeFile(
      settings,
      dotenv({
        ...example_profile,
        LOGTO_CLIENT_SECRET: 'private',
        TAURI_UNRELATED_KEY: 'private',
      }),
    );
    const environment: NodeJS.ProcessEnv = {};

    await loadShellAccountEnvironment(environment, settings);

    expect(environment).toEqual({
      ...example_profile,
      ...Object.fromEntries(
        Object.entries(example_profile).map(([key, value]) => [`TAURI_${key}`, value]),
      ),
    });
  });

  it('prefers explicit build settings, including an explicit empty value', async () => {
    await writeFile(
      settings,
      [
        'APELL_ACCOUNT_ISSUER=https://account.example.invalid/oidc',
        'APELL_ACCOUNT_CLIENT_ID=from-file',
        'APELL_SYNC_RESOURCE=https://store.example.invalid/config',
      ].join('\n'),
    );
    const environment: NodeJS.ProcessEnv = {
      APELL_ACCOUNT_ISSUER: '',
      TAURI_APELL_ACCOUNT_ISSUER: 'https://alias.example.invalid/oidc',
      TAURI_APELL_ACCOUNT_CLIENT_ID: 'from-env',
    };

    await loadShellAccountEnvironment(environment, settings);

    expect(environment).toEqual({
      APELL_ACCOUNT_ISSUER: '',
      TAURI_APELL_ACCOUNT_ISSUER: '',
      APELL_ACCOUNT_CLIENT_ID: 'from-env',
      TAURI_APELL_ACCOUNT_CLIENT_ID: 'from-env',
      APELL_SYNC_RESOURCE: 'https://store.example.invalid/config',
      TAURI_APELL_SYNC_RESOURCE: 'https://store.example.invalid/config',
    });
  });

  it('leaves Account optional when the local profile and exported settings are absent', async () => {
    const environment = { OTHER_SETTING: 'keep' };

    await loadShellAccountEnvironment(environment, settings);

    expect(environment).toEqual({ OTHER_SETTING: 'keep' });
  });

  it('requires a relay setting for configured builds', async () => {
    await writeFile(settings, dotenv({ ...example_profile, APELL_KEY_EXCHANGE_RESOURCE: '' }));

    await expect(
      loadShellAccountEnvironment({ APELL_REQUIRE_KEY_SYNC: '1' }, settings),
    ).rejects.toThrow('Missing shell build profile settings (APELL_KEY_EXCHANGE_RESOURCE).');
  });

  it('uses the complete explicit profile for all targets and forwards only its allowlist', async () => {
    const environment: NodeJS.ProcessEnv = {
      APELL_SHELL_BUILD_PROFILE: dotenv({ ...example_profile, LOGTO_CLIENT_SECRET: 'private' }),
      APELL_ACCOUNT_ISSUER: 'https://wrong.example.invalid/oidc',
      TAURI_APELL_ACCOUNT_ISSUER: 'https://wrong-alias.example.invalid/oidc',
    };

    await loadShellAccountEnvironment(environment, settings);

    expect(environment.APELL_ACCOUNT_ISSUER).toBe(example_profile.APELL_ACCOUNT_ISSUER);
    expect(environment.TAURI_APELL_ACCOUNT_ISSUER).toBe(example_profile.APELL_ACCOUNT_ISSUER);
    expect(environment.APELL_SHELL_BUILD_PROFILE).toBeUndefined();
    expect(environment.LOGTO_CLIENT_SECRET).toBeUndefined();
  });

  it('rejects an empty configured CI profile without falling back to other settings', async () => {
    await writeFile(settings, dotenv(example_profile));

    await expect(
      loadShellAccountEnvironment({ APELL_SHELL_BUILD_PROFILE: '' }, settings),
    ).rejects.toThrow('Missing shell build profile settings');
  });

  it.each([
    ['APELL_ACCOUNT_CLIENT_ID', 'client with spaces'],
    ['APELL_ACCOUNT_ISSUER', 'http://account.example.invalid/oidc'],
    ['APELL_SYNC_URL', 'https://store.example.invalid/api'],
    ['APELL_SYNC_RESOURCE', 'https://store.example.invalid/config?'],
    ['APELL_KEY_EXCHANGE_RESOURCE', 'https://other.example.invalid/relay'],
  ])('rejects invalid %s without printing its value', async (name, value) => {
    const environment = {
      APELL_SHELL_BUILD_PROFILE: dotenv({ ...example_profile, [name]: value }),
    };

    const failure = await loadShellAccountEnvironment(environment, settings).catch(
      (error: Error) => error,
    );
    expect(failure).toBeInstanceOf(Error);
    expect(failure?.message).toContain(name);
    expect(failure?.message).not.toContain(value);
  });
});

it.each([
  [['ios', 'build', '--target', 'aarch64'], 'ios'],
  [['ios', 'build', '--target', 'aarch64-sim'], 'ios'],
  [['android', 'build', '--apk'], 'android'],
  [['build', '--bundles', 'app'], 'desktop'],
  [['build', '--help'], undefined],
  [['ios', 'xcode-script'], undefined],
] as const)('selects the shell profile for %j', (args, target) => {
  expect(shellBuildTarget(args)).toBe(target);
});

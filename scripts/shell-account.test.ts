import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { loadShellAccountEnvironment } from './shell-account.ts';

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
    const values = {
      APELL_ACCOUNT_ISSUER: 'https://account.example.invalid/oidc',
      APELL_ACCOUNT_CLIENT_ID: 'shell-device',
      APELL_SYNC_URL: 'https://store.example.invalid',
      APELL_SYNC_RESOURCE: 'https://store.example.invalid/config',
      APELL_KEY_EXCHANGE_RESOURCE: 'https://account.example.invalid/key-exchange',
    };
    await writeFile(
      settings,
      [
        ...Object.entries(values).map(([key, value]) => `${key}="${value}"`),
        'LOGTO_CLIENT_SECRET=private',
        'TAURI_UNRELATED_KEY=private',
      ].join('\n'),
    );
    const environment: NodeJS.ProcessEnv = {};

    await loadShellAccountEnvironment(environment, settings);

    expect(environment).toEqual({
      ...values,
      ...Object.fromEntries(Object.entries(values).map(([key, value]) => [`TAURI_${key}`, value])),
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
});

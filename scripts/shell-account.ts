import { readFile } from 'node:fs/promises';
import { parseEnv } from 'node:util';

const account_variables = [
  'APELL_ACCOUNT_ISSUER',
  'APELL_ACCOUNT_CLIENT_ID',
  'APELL_SYNC_URL',
  'APELL_SYNC_RESOURCE',
  'APELL_KEY_EXCHANGE_RESOURCE',
];

export async function loadShellAccountEnvironment(
  environment: NodeJS.ProcessEnv = process.env,
  settings_path = new URL('../deploy/account/.env.shell-pilot', import.meta.url),
): Promise<void> {
  const settings = await readFile(settings_path, 'utf8').catch((error: NodeJS.ErrnoException) => {
    if (error.code === 'ENOENT') return '';
    throw error;
  });
  const local = parseEnv(settings);
  for (const name of account_variables) {
    const tauri_name = `TAURI_${name}`;
    const value = environment[name] ?? environment[tauri_name] ?? local[name];
    if (value === undefined) continue;
    environment[name] = value;
    // Tauri forwards only prefixed values to the native mobile build scripts.
    environment[tauri_name] = value;
  }
}

import { execFileSync } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const script_path = fileURLToPath(import.meta.url);
const repository_root = resolve(dirname(script_path), '..');
const icon_names = ['recorder', 'learning', 'fieldnotes'] as const;

export function appsRoot(root: string, override = process.env.APELL_APPS_ROOT): string {
  if (override) return resolve(override);
  const common_directory = execFileSync(
    'git',
    ['rev-parse', '--path-format=absolute', '--git-common-dir'],
    { cwd: root, encoding: 'utf8' },
  ).trim();
  return resolve(dirname(common_directory), '..', 'apps');
}

export async function syncAppIcons(root: string, apps_root: string): Promise<void> {
  const icons = await Promise.all(
    icon_names.map(async (name) => ({
      name,
      source: await readFile(join(apps_root, 'shared', 'assets', `${name}.svg`)),
    })),
  );
  const destination = join(root, 'site', 'applets');
  await mkdir(destination, { recursive: true });
  await Promise.all(
    icons.map(({ name, source }) => writeFile(join(destination, `${name}.svg`), source)),
  );
}

if (process.argv[1] && resolve(process.argv[1]) === script_path) {
  try {
    await syncAppIcons(repository_root, appsRoot(repository_root));
    console.log(
      'Synced Recorder, Learning, and Fieldnotes website icons from the apps repository.',
    );
  } catch (error) {
    throw new Error(
      'Could not sync applet icons. Set APELL_APPS_ROOT to an apps checkout containing shared/assets.',
      { cause: error },
    );
  }
}

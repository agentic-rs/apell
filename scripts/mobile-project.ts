import { execFileSync } from 'node:child_process';
import { lstat, readFile, rename, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const project_directory = fileURLToPath(new URL('..', import.meta.url));
const app_name = 'apell-app';
const old_target = 'apell_iOS';
const new_target = `${app_name}_iOS`;

async function entry(path: string) {
  try {
    return await lstat(path);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw error;
  }
}

export async function migrateIosProjectName(project_root = project_directory): Promise<void> {
  const apple = join(project_root, 'src-tauri', 'gen', 'apple');
  const old_project = join(apple, 'apell.xcodeproj');
  const old_entry = await entry(old_project);
  if (!old_entry) return;
  const schemes = join(old_project, 'xcshareddata', 'xcschemes');
  const scheme = join(schemes, `${old_target}.xcscheme`);
  const target = join(apple, old_target);
  const moves = [
    {
      from: join(target, `${old_target}.entitlements`),
      to: join(target, `${new_target}.entitlements`),
    },
    { from: scheme, to: join(schemes, `${new_target}.xcscheme`) },
    { from: target, to: join(apple, new_target) },
    { from: old_project, to: join(apple, `${app_name}.xcodeproj`) },
  ];
  try {
    execFileSync('git', ['check-ignore', '--quiet', '--', 'src-tauri/gen/apple/project.yml'], {
      cwd: project_root,
      stdio: 'ignore',
    });
  } catch {
    throw new Error('The generated iOS project must be ignored by Git before renaming it.');
  }
  if (!old_entry.isDirectory() || !(await entry(schemes))?.isDirectory()) {
    throw new Error('The legacy iOS project is missing its shared scheme directory.');
  }
  for (const { from, to } of moves) {
    const source = await entry(from);
    const valid_source =
      from === target || from === old_project ? source?.isDirectory() : source?.isFile();
    if (!valid_source || (await entry(to))) {
      throw new Error('The legacy iOS project has missing files or conflicting apell-app paths.');
    }
  }

  const yaml = join(apple, 'project.yml');
  const pbx = join(old_project, 'project.pbxproj');
  const podfile = join(apple, 'Podfile');
  const names = (value: string) => value.replaceAll(old_target, new_target);
  const edits = [
    {
      path: yaml,
      transform: (value: string) =>
        names(value)
          .replace(/^name: apell(?=[ \t]*\r?$)/m, `name: ${app_name}`)
          .replace(
            /^([ \t]*PRODUCT_NAME:[ \t]+)(?:"Apell"|'Apell'|Apell)(?=[ \t]*\r?$)/gm,
            `$1${app_name}`,
          ),
    },
    {
      path: pbx,
      transform: (value: string) =>
        names(value)
          .replaceAll('Apell.app', `${app_name}.app`)
          .replaceAll('for PBXProject "apell"', `for PBXProject "${app_name}"`)
          .replace(/(PRODUCT_NAME = )"?Apell"?;/g, `$1"${app_name}";`),
    },
    {
      path: scheme,
      transform: (value: string) =>
        names(value)
          .replaceAll('container:apell.xcodeproj', `container:${app_name}.xcodeproj`)
          .replaceAll('BuildableName = "Apell.app"', `BuildableName = "${app_name}.app"`),
    },
  ];
  if (await entry(podfile)) {
    edits.push({
      path: podfile,
      transform: (value) => names(value).replaceAll('apell_macOS', `${app_name}_macOS`),
    });
  }

  // Read and validate the complete migration before changing local signing files.
  const changes = await Promise.all(
    edits.map(async ({ path, transform }) => {
      if (!(await entry(path))?.isFile()) throw new Error('Missing a generated iOS project file.');
      const original = await readFile(path, 'utf8');
      const updated = transform(original);
      return { path, original, updated };
    }),
  );
  if (changes.slice(0, 3).some(({ original }) => !original.includes(old_target))) {
    throw new Error('The generated iOS files do not match the legacy Apell target.');
  }

  const written: typeof changes = [];
  const renamed: typeof moves = [];
  try {
    for (const change of changes) {
      if (change.updated !== change.original) {
        written.push(change);
        await writeFile(change.path, change.updated);
      }
    }
    for (const move of moves) {
      await rename(move.from, move.to);
      renamed.push(move);
    }
  } catch (error) {
    for (const { from, to } of renamed.reverse()) await rename(to, from);
    for (const { path, original } of written.reverse()) await writeFile(path, original);
    throw new Error('Could not rename the generated iOS project.', { cause: error });
  }
}

export async function syncAndroidAppName(project_root = project_directory): Promise<void> {
  const path = join(
    project_root,
    'src-tauri',
    'gen',
    'android',
    'app',
    'src',
    'main',
    'res',
    'values',
    'strings.xml',
  );
  const existing = await entry(path);
  if (!existing) return;
  if (!existing.isFile()) throw new Error('Expected a generated Android strings.xml file.');
  const original = await readFile(path, 'utf8');
  const renamed = original.replace(
    /(<string name="(?:app_name|main_activity_title)">)("?)Apell\2(<\/string>)/g,
    `$1$2${app_name}$2$3`,
  );
  if (renamed !== original) await writeFile(path, renamed);
}

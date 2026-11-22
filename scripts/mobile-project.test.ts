import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, expect, it } from 'vitest';
import { migrateIosProjectName, syncAndroidAppName } from './mobile-project.ts';

const directories: string[] = [];

async function temporaryProject() {
  const directory = await mkdtemp(join(tmpdir(), 'apell-mobile-name-'));
  directories.push(directory);
  await writeFile(join(directory, '.gitignore'), 'src-tauri/gen/\n');
  execFileSync('git', ['init', '--quiet'], { cwd: directory, stdio: 'pipe' });
  return directory;
}

afterEach(async () => {
  await Promise.all(
    directories.splice(0).map((directory) => rm(directory, { recursive: true, force: true })),
  );
});

async function legacyIosProject() {
  const project = await temporaryProject();
  const apple = join(project, 'src-tauri', 'gen', 'apple');
  const target = join(apple, 'apell_iOS');
  const xcode = join(apple, 'apell.xcodeproj');
  const schemes = join(xcode, 'xcshareddata', 'xcschemes');
  const pbx =
    'CODE_SIGN_ENTITLEMENTS = apell_iOS/apell_iOS.entitlements;\nPRODUCT_NAME = "Apell";\nDEVELOPMENT_TEAM = FIXTURETEAM;\nPRODUCT_BUNDLE_IDENTIFIER = dev.apell.shell;\n';
  await Promise.all([mkdir(target, { recursive: true }), mkdir(schemes, { recursive: true })]);
  await Promise.all([
    writeFile(
      join(apple, 'project.yml'),
      'name: apell\nPRODUCT_NAME: Apell\ntargets:\n  apell_iOS:\n',
    ),
    writeFile(join(apple, 'Podfile'), "target 'apell_iOS' do\nend\n"),
    writeFile(join(xcode, 'project.pbxproj'), pbx, { mode: 0o600 }),
    writeFile(
      join(schemes, 'apell_iOS.xcscheme'),
      '<BuildableReference BlueprintName = "apell_iOS" BuildableName = "Apell.app" ReferencedContainer = "container:apell.xcodeproj" />',
    ),
    writeFile(join(target, 'apell_iOS.entitlements'), 'fixture entitlements'),
    writeFile(join(target, 'Info.plist'), 'fixture Info.plist'),
  ]);
  return { project, apple, xcode, pbx };
}

it('renames the generated iOS target and shared scheme while preserving signing and entitled files', async () => {
  const { project, apple, xcode } = await legacyIosProject();
  await migrateIosProjectName(project);
  await migrateIosProjectName(project);
  await expect(stat(xcode)).rejects.toMatchObject({ code: 'ENOENT' });
  const migrated = join(apple, 'apell-app.xcodeproj');
  const pbx = join(migrated, 'project.pbxproj');
  expect(await readFile(pbx, 'utf8')).toBe(
    'CODE_SIGN_ENTITLEMENTS = apell-app_iOS/apell-app_iOS.entitlements;\nPRODUCT_NAME = "apell-app";\nDEVELOPMENT_TEAM = FIXTURETEAM;\nPRODUCT_BUNDLE_IDENTIFIER = dev.apell.shell;\n',
  );
  expect((await stat(pbx)).mode & 0o777).toBe(0o600);
  expect(await readFile(join(apple, 'project.yml'), 'utf8')).toBe(
    'name: apell-app\nPRODUCT_NAME: apell-app\ntargets:\n  apell-app_iOS:\n',
  );
  expect(await readFile(join(apple, 'Podfile'), 'utf8')).toContain("target 'apell-app_iOS'");
  const scheme = join(migrated, 'xcshareddata', 'xcschemes', 'apell-app_iOS.xcscheme');
  expect(await readFile(scheme, 'utf8')).toBe(
    '<BuildableReference BlueprintName = "apell-app_iOS" BuildableName = "apell-app.app" ReferencedContainer = "container:apell-app.xcodeproj" />',
  );
  expect(await readFile(join(apple, 'apell-app_iOS', 'apell-app_iOS.entitlements'), 'utf8')).toBe(
    'fixture entitlements',
  );
  expect(await readFile(join(apple, 'apell-app_iOS', 'Info.plist'), 'utf8')).toBe(
    'fixture Info.plist',
  );
});

it('checks for conflicting destination paths before modifying the legacy project', async () => {
  const { project, apple, xcode, pbx } = await legacyIosProject();
  await mkdir(join(apple, 'apell-app.xcodeproj'));
  await expect(migrateIosProjectName(project)).rejects.toThrow('conflicting apell-app paths');
  expect(await readFile(join(xcode, 'project.pbxproj'), 'utf8')).toBe(pbx);
});

it('updates the generated Android labels without replacing unrelated or customized strings', async () => {
  const project = await temporaryProject();
  const values = join(
    project,
    'src-tauri',
    'gen',
    'android',
    'app',
    'src',
    'main',
    'res',
    'values',
  );
  await mkdir(values, { recursive: true });
  const resource = join(values, 'strings.xml');
  const original =
    '<resources>\n<string name="app_name">"Apell"</string>\n<string name="main_activity_title">Apell</string>\n<string name="another">Apell</string>\n</resources>\n';
  await writeFile(resource, original);
  await syncAndroidAppName(project);
  await syncAndroidAppName(project);
  expect(await readFile(resource, 'utf8')).toBe(
    original.replace('>"Apell"<', '>"apell-app"<').replace('>Apell<', '>apell-app<'),
  );
  const customized = original.replace('>"Apell"<', '>"Local label"<');
  await writeFile(resource, customized);
  await syncAndroidAppName(project);
  expect(await readFile(resource, 'utf8')).toBe(customized.replace('>Apell<', '>apell-app<'));
});

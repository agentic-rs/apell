import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { appsRoot, syncAppIcons } from './sync-app-icons';

const temporary_roots: string[] = [];
afterEach(async () => {
  await Promise.all(
    temporary_roots.splice(0).map((root) => rm(root, { recursive: true, force: true })),
  );
});

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'apell-icon-sync-'));
  temporary_roots.push(root);
  const host = join(root, 'apell');
  const apps = join(root, 'apps');
  await mkdir(host);
  await mkdir(join(apps, 'shared', 'assets'), { recursive: true });
  return { root, host, apps };
}

describe('website applet artwork sync', () => {
  it('resolves the primary sibling apps checkout from a linked host worktree', async () => {
    const { root, host, apps } = await fixture();
    execFileSync('git', ['init', '--quiet', host]);
    execFileSync(
      'git',
      [
        '-c',
        'user.name=Test',
        '-c',
        'user.email=test@example.invalid',
        'commit',
        '--quiet',
        '--allow-empty',
        '-m',
        'test: fixture',
      ],
      { cwd: host },
    );
    const worktree = join(root, 'host-worktree');
    execFileSync('git', ['worktree', 'add', '--quiet', '--detach', worktree], { cwd: host });
    expect(appsRoot(host, '')).toBe(await realpath(apps));
    expect(appsRoot(worktree, '')).toBe(await realpath(apps));
    expect(appsRoot(worktree, join(root, 'alternate-apps'))).toBe(join(root, 'alternate-apps'));
  });

  it('copies canonical artwork exactly and reads every source before replacing files', async () => {
    const { host, apps } = await fixture();
    const destination = join(host, 'site', 'applets');
    await mkdir(destination, { recursive: true });
    await writeFile(join(destination, 'recorder.svg'), 'previous');
    await writeFile(join(apps, 'shared', 'assets', 'recorder.svg'), '<svg>recorder</svg>');
    await expect(syncAppIcons(host, apps)).rejects.toThrow();
    expect(await readFile(join(destination, 'recorder.svg'), 'utf8')).toBe('previous');
    for (const name of ['learning', 'fieldnotes']) {
      await writeFile(join(apps, 'shared', 'assets', `${name}.svg`), `<svg>${name}</svg>`);
    }
    await syncAppIcons(host, apps);
    for (const name of ['recorder', 'learning', 'fieldnotes']) {
      expect(await readFile(join(destination, `${name}.svg`), 'utf8')).toBe(`<svg>${name}</svg>`);
    }
  });
});

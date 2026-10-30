import { execFileSync } from 'node:child_process';
import {
  chmod,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  realpath,
  rm,
  symlink,
  writeFile,
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import {
  buildEnvironment,
  patchPbxProject,
  patchProjectYaml,
  prepareIos,
  resolveToolDirectories,
} from './prepare-ios.ts';

const rust_command =
  'pnpm tauri ios xcode-script -v --platform "${PLATFORM_DISPLAY_NAME:?}" ${ARCHS:?}';
const project_yaml = `name: fixture
settings:
  base:
    DEVELOPMENT_TEAM: TESTTEAM00
    CODE_SIGN_STYLE: Manual
targets:
  fixture_iOS:
    preBuildScripts:
      - script: |
          ${rust_command}
        name: Build Rust Code
        basedOnDependencyAnalysis: false
        outputFiles:
          - $(SRCROOT)/Externals/arm64/libapp.a
      - script: |
          printf '%s' "independent phase"
        name: Preserve This Phase
`;
const project_pbx = `// !$*UTF8*$!
{
  objects = {
/* Begin PBXShellScriptBuildPhase section */
    111111111111111111111111 /* Build Rust Code */ = {
      isa = PBXShellScriptBuildPhase;
      name = "Build Rust Code";
      outputPaths = ("$(SRCROOT)/Externals/arm64/libapp.a");
      shellPath = /bin/sh;
      shellScript = ${JSON.stringify(`${rust_command}\n`)};
    };
    222222222222222222222222 /* Preserve This Phase */ = {
      isa = PBXShellScriptBuildPhase;
      name = "Preserve This Phase";
      shellPath = /bin/sh;
      shellScript = "printf '%s' independent";
    };
/* End PBXShellScriptBuildPhase section */
    333333333333333333333333 /* Release */ = {
      isa = XCBuildConfiguration;
      buildSettings = {
        DEVELOPMENT_TEAM = TESTTEAM00;
        CODE_SIGN_STYLE = Manual;
      };
    };
  };
}
`;

const temporary_directories: string[] = [];

async function temporaryDirectory() {
  const directory = await mkdtemp(join(tmpdir(), 'apell-ios-prepare-'));
  temporary_directories.push(directory);
  return directory;
}

afterEach(async () => {
  await Promise.all(
    temporary_directories
      .splice(0)
      .map((directory) => rm(directory, { recursive: true, force: true })),
  );
});

async function executable(path: string, output: string) {
  await writeFile(path, `#!/bin/sh\nprintf '%s' '${output}'\n`);
  await chmod(path, 0o755);
}

async function projectFixture() {
  const project_root = await temporaryDirectory();
  const apple_directory = join(project_root, 'src-tauri/gen/apple');
  const xcode_directory = join(apple_directory, 'fixture.xcodeproj');
  const tools_directory = join(project_root, "developer's tools");
  await Promise.all([mkdir(xcode_directory, { recursive: true }), mkdir(tools_directory)]);
  await Promise.all([
    writeFile(join(project_root, '.gitignore'), 'src-tauri/gen/\n'),
    writeFile(join(apple_directory, 'project.yml'), project_yaml),
    writeFile(join(xcode_directory, 'project.pbxproj'), project_pbx),
    ...['node', 'pnpm', 'cargo', 'rustup'].map((name) =>
      executable(join(tools_directory, name), name),
    ),
  ]);
  execFileSync('git', ['init', '--quiet'], { cwd: project_root, stdio: 'pipe' });
  return {
    apple_directory,
    yaml_path: join(apple_directory, 'project.yml'),
    pbx_path: join(xcode_directory, 'project.pbxproj'),
    options: {
      project_root,
      search_path: tools_directory,
      node_executable: join(tools_directory, 'node'),
    },
  };
}

async function snapshot(directory: string): Promise<Record<string, string>> {
  const result: Record<string, string> = {};
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      for (const [name, contents] of Object.entries(await snapshot(path))) {
        result[`${entry.name}/${name}`] = contents;
      }
    } else {
      result[entry.name] = await readFile(path, 'utf8');
    }
  }
  return result;
}

describe('iOS GUI build preparation', () => {
  it('patches YAML once while preserving signing settings and other build phases byte for byte', () => {
    const patched = patchProjectYaml(project_yaml);
    expect(patched).not.toBe(project_yaml);
    expect(patchProjectYaml(patched)).toBe(patched);
    const script_pattern = /(      - script: \|\n)([\s\S]*?)(        name: Build Rust Code)/;
    const patched_script = patched.match(script_pattern)?.[2];
    expect(patched_script).toContain(`          ${rust_command}\n`);
    expect(patched.replace(script_pattern, `$1          ${rust_command}\n$3`)).toBe(project_yaml);
  });

  it('patches the PBX Rust phase once while preserving signing settings and unrelated phases byte for byte', () => {
    const patched = patchPbxProject(project_pbx);
    expect(patched).not.toBe(project_pbx);
    expect(patchPbxProject(patched)).toBe(patched);
    const script_pattern =
      /(\/\* Build Rust Code \*\/ = \{[\s\S]*?shellScript = )("(?:\\.|[^"\\])*")/;
    const encoded_script = patched.match(script_pattern)?.[2];
    expect(encoded_script).toBeDefined();
    expect(JSON.parse(encoded_script!)).toContain(`${rust_command}\n`);
    expect(
      patched.replace(
        script_pattern,
        (_match, prefix: string) => `${prefix}${JSON.stringify(`${rust_command}\n`)}`,
      ),
    ).toBe(project_pbx);
  });

  it('quotes tool directories as shell data and preserves the PATH supplied by Xcode', async () => {
    const directory = await temporaryDirectory();
    const tool_directories = [
      join(directory, "developer's node tools"),
      join(directory, 'rust $tools `literal`'),
    ];
    const xcode_path = join(directory, 'Xcode tools');
    const path = execFileSync(
      '/bin/sh',
      ['-c', `${buildEnvironment(tool_directories)}\nprintf '%s' "$PATH"`],
      {
        env: { PATH: xcode_path },
        encoding: 'utf8',
      },
    );
    expect(path).toBe([...tool_directories, xcode_path].join(':'));
  });

  it('resolves a symlinked PATH directory without replacing the pnpm shim directory with its payload directory', async () => {
    const directory = await temporaryDirectory();
    const node_directory = join(directory, 'node tools');
    const package_directory = join(directory, 'package tools');
    const rust_directory = join(directory, 'rust tools');
    const payload_directory = join(directory, 'package payload');
    const alias_directory = join(directory, 'package alias');
    await Promise.all(
      [node_directory, package_directory, rust_directory, payload_directory].map((path) =>
        mkdir(path),
      ),
    );
    await Promise.all([
      executable(join(node_directory, 'node'), 'node'),
      executable(join(payload_directory, 'pnpm-entry'), 'pnpm'),
      executable(join(rust_directory, 'cargo'), 'cargo'),
      executable(join(rust_directory, 'rustup'), 'rustup'),
    ]);
    await symlink(join(payload_directory, 'pnpm-entry'), join(package_directory, 'pnpm'));
    await symlink(package_directory, alias_directory, 'dir');
    const tool_directories = await resolveToolDirectories(
      [join(directory, 'absent'), alias_directory, rust_directory].join(':'),
      join(node_directory, 'node'),
    );
    expect(new Set(tool_directories)).toEqual(
      new Set(
        await Promise.all(
          [node_directory, package_directory, rust_directory].map((path) => realpath(path)),
        ),
      ),
    );
    const found = execFileSync(
      '/bin/sh',
      ['-c', `${buildEnvironment(tool_directories)}\nnode; pnpm; cargo; rustup`],
      {
        env: { PATH: '/usr/bin:/bin' },
        encoding: 'utf8',
      },
    );
    expect(found).toBe('nodepnpmcargorustup');
  });

  it('prepares an ignored generated project idempotently without changing its signing configuration', async () => {
    const fixture = await projectFixture();
    await prepareIos(fixture.options);
    const prepared = await snapshot(fixture.apple_directory);
    expect(prepared['.apell-build-env']).toContain('PATH');
    expect(prepared['project.yml']).toBe(patchProjectYaml(project_yaml));
    expect(prepared['fixture.xcodeproj/project.pbxproj']).toBe(patchPbxProject(project_pbx));
    await prepareIos(fixture.options);
    expect(await snapshot(fixture.apple_directory)).toEqual(prepared);
  });

  it.each([
    [
      'YAML phase missing',
      'yaml_path',
      project_yaml.replace('name: Build Rust Code', 'name: Unexpected Phase'),
    ],
    ['YAML command changed', 'yaml_path', project_yaml.replace(rust_command, 'echo custom-build')],
    [
      'PBX phase missing',
      'pbx_path',
      project_pbx.replaceAll('Build Rust Code', 'Unexpected Phase'),
    ],
    [
      'PBX command changed',
      'pbx_path',
      project_pbx.replace(
        JSON.stringify(`${rust_command}\n`),
        JSON.stringify('echo custom-build\n'),
      ),
    ],
  ] as const)(
    'rejects %s before mutating any generated file',
    async (_case, path_key, contents) => {
      const fixture = await projectFixture();
      await writeFile(fixture[path_key], contents);
      await writeFile(
        join(fixture.apple_directory, '.apell-build-env'),
        '# Existing environment must survive validation failure.\n',
      );
      const before = await snapshot(fixture.apple_directory);
      await expect(prepareIos(fixture.options)).rejects.toThrow();
      expect(await snapshot(fixture.apple_directory)).toEqual(before);
    },
  );
});

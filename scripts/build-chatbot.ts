import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';
import { build } from 'esbuild';

const source = 'examples/chatbot';
const output = 'artifacts/chatbot.apel';
const compilerFlags =
  process.env.CARGO_ENCODED_RUSTFLAGS?.split('\x1f') ??
  (process.env.RUSTFLAGS ?? '').split(/\s+/).filter(Boolean);
execFileSync(
  'cargo',
  [
    'build',
    '--locked',
    '--release',
    '--target',
    'wasm32-unknown-unknown',
    '--manifest-path',
    `${source}/Cargo.toml`,
  ],
  {
    stdio: 'inherit',
    env: {
      ...process.env,
      // Rust panic locations are part of the distributed guest binary.
      CARGO_ENCODED_RUSTFLAGS: [
        ...compilerFlags.filter(Boolean),
        `--remap-path-prefix=${homedir()}=/build`,
        `--remap-path-prefix=${process.cwd()}=/apell`,
      ].join('\x1f'),
    },
  },
);
const staging = await mkdtemp(join(tmpdir(), 'apell-companion-'));
try {
  await mkdir(join(staging, 'lib'));
  await copyFile(
    `${source}/target/wasm32-unknown-unknown/release/chatbot.wasm`,
    join(staging, 'lib/main.wasm'),
  );
  await copyFile(`${source}/manifest.json`, join(staging, 'manifest.json'));
  await copyFile(`${source}/index.html`, join(staging, 'index.html'));
  await build({
    entryPoints: [`${source}/src/main.ts`],
    bundle: true,
    format: 'iife',
    target: 'es2022',
    minify: true,
    outdir: join(staging, 'assets'),
  });
  await mkdir('artifacts', { recursive: true });
  execFileSync(
    'cargo',
    [
      'run',
      '--quiet',
      '--locked',
      '-p',
      'apell-cli',
      '--bin',
      'apell',
      '--',
      'pack',
      staging,
      '--output',
      output,
    ],
    { stdio: 'inherit' },
  );
} finally {
  await rm(staging, { recursive: true, force: true });
}
const sha256 = createHash('sha256')
  .update(await readFile(output))
  .digest('hex');
await writeFile('artifacts/chatbot.sha256', `${sha256}  chatbot.apel\n`);
await mkdir('src/generated', { recursive: true });
await copyFile(output, 'src/generated/chatbot.apel');
await writeFile(
  'src/generated/chatbot.ts',
  `export const sample_base64 = '${(await readFile(output)).toString('base64')}';\n`,
);
console.log(`Companion package ready. SHA-256: ${sha256}`);

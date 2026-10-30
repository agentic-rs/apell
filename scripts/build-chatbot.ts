import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { build } from 'esbuild';

execFileSync(
  'cargo',
  [
    'build',
    '--locked',
    '--release',
    '--target',
    'wasm32-unknown-unknown',
    '--manifest-path',
    'examples/chatbot/Cargo.toml',
  ],
  { stdio: 'inherit' },
);
execFileSync(
  'wasm-bindgen',
  [
    'examples/chatbot/target/wasm32-unknown-unknown/release/chatbot.wasm',
    '--target',
    'web',
    '--out-dir',
    'examples/chatbot/pkg',
    '--out-name',
    'chatbot',
  ],
  { stdio: 'inherit' },
);
const wasm = await readFile('examples/chatbot/pkg/chatbot_bg.wasm');
const output = await build({
  entryPoints: ['examples/chatbot/src/main.ts'],
  bundle: true,
  write: false,
  format: 'iife',
  target: 'es2022',
  minify: true,
  outdir: 'artifacts/chatbot',
  define: { 'import.meta.url': '"apell:embedded"' },
});
let javascript = output.outputFiles.find((file) => file.path.endsWith('.js'))!.text;
javascript = javascript.replace('__WASM_BASE64__', wasm.toString('base64'));
const css = output.outputFiles.find((file) => file.path.endsWith('.css'))?.text ?? '';
const html = `<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover"><title>Companion</title><style>${css.replace(/<\/style/gi, '<\\/style')}</style></head><body><div id="app"></div><script>${javascript.replace(/<\/script/gi, '<\\/script')}</script></body></html>`;
const bundle = JSON.stringify({
  manifest: {
    schema_version: 1,
    api_version: 1,
    applet_id: 'chatbot',
    name: 'Companion',
    version: '0.1.0',
    permissions: ['sqlite', 'llm'],
  },
  html,
});
const sha256 = createHash('sha256').update(bundle).digest('hex');
await mkdir('artifacts', { recursive: true });
await mkdir('src/generated', { recursive: true });
await writeFile('artifacts/chatbot.apell.json', bundle);
await writeFile('artifacts/chatbot.sha256', `${sha256}  chatbot.apell.json\n`);
await writeFile('src/generated/chatbot.apell.json', bundle);
console.log(
  `Chatbot bundle ready (${Math.round(Buffer.byteLength(bundle) / 1024)} KiB). SHA-256: ${sha256}`,
);

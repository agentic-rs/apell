import { spawnSync } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import {
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const local = join(root, 'deploy/local');
const state = join(local, '.state');
const workspace = join(state, 'source');
const config = join(state, 'config');
const tls = join(config, 'tls');
const checkout = createHash('sha256').update(root).digest('hex').slice(0, 12);
const name = `apell-local-${checkout}`;
const image = `${name}:dev`;
const engine = process.env.APELL_LOCAL_ENGINE || 'docker';
const issuer = 'https://localhost:5656/oidc';
const store = 'https://localhost:8788';
const store_resource = `${store}/api/v1/config-sync`;
const key_resource = 'https://localhost:5656/api/v1/key-vault';
const shell_client = 'apell-local-shell';

function run(program: string, args: string[], env = process.env) {
  const result = spawnSync(program, args, { cwd: root, stdio: 'inherit', env });
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw new Error(`${program} exited with ${result.status ?? result.signal}.`);
}

function output(program: string, args: string[]) {
  return spawnSync(program, args, { cwd: root, encoding: 'utf8' });
}

function save(name: string, value: string) {
  const path = join(config, name);
  if (!existsSync(path)) writeFileSync(path, `${value}\n`, { flag: 'wx', mode: 0o600 });
}

function certificate() {
  const ca = join(tls, 'ca.crt');
  const ca_key = join(state, 'ca.key');
  const cert = join(tls, 'localhost.crt');
  const cert_key = join(tls, 'localhost.key');
  if (!existsSync(ca) && !existsSync(ca_key)) {
    run('openssl', [
      'req',
      '-x509',
      '-newkey',
      'rsa:2048',
      '-nodes',
      '-sha256',
      '-days',
      '365',
      '-keyout',
      ca_key,
      '-out',
      ca,
      '-subj',
      '/CN=Apell Local Development CA',
      '-addext',
      'basicConstraints=critical,CA:TRUE,pathlen:0',
      '-addext',
      'keyUsage=critical,keyCertSign,cRLSign',
    ]);
  }
  if (
    existsSync(cert) &&
    existsSync(cert_key) &&
    output('openssl', ['x509', '-checkend', '86400', '-noout', '-in', cert]).status === 0
  )
    return;
  const csr = join(tls, 'localhost.csr');
  const extensions = join(tls, 'localhost.ext');
  writeFileSync(
    extensions,
    'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n',
  );
  run('openssl', [
    'req',
    '-newkey',
    'rsa:2048',
    '-nodes',
    '-keyout',
    cert_key,
    '-out',
    csr,
    '-subj',
    '/CN=localhost',
  ]);
  run('openssl', [
    'x509',
    '-req',
    '-sha256',
    '-days',
    '30',
    '-in',
    csr,
    '-CA',
    ca,
    '-CAkey',
    ca_key,
    '-set_serial',
    `0x${randomBytes(16).toString('hex')}`,
    '-out',
    cert,
    '-extfile',
    extensions,
  ]);
  rmSync(csr);
  rmSync(extensions);
}

function initialize() {
  process.umask(0o077);
  mkdirSync(tls, { recursive: true, mode: 0o700 });
  certificate();
  const jwt_key = join(config, 'identity-key.pem');
  if (!existsSync(jwt_key)) run('openssl', ['genrsa', '-out', jwt_key, '2048']);
  const secret = () => randomBytes(32).toString('base64url');
  save('publish-token', secret());
  save('master.key', secret());
  save(
    'store-oidc.json',
    JSON.stringify({ issuer, client_id: 'apell-local-store', client_secret: secret() }),
  );
  save(
    'recovery-oidc.json',
    JSON.stringify({ client_id: 'apell-local-recovery', client_secret: secret() }),
  );
  save(
    'store-sync.json',
    JSON.stringify({ issuer, audience: store_resource, client_id: shell_client }),
  );
  save(
    'relay-oidc.json',
    JSON.stringify({ issuer, audience: key_resource, client_id: shell_client }),
  );
  save(
    'shell.env',
    [
      `APELL_ACCOUNT_ISSUER=${issuer}`,
      `APELL_ACCOUNT_CLIENT_ID=${shell_client}`,
      `APELL_SYNC_URL=${store}`,
      `APELL_SYNC_RESOURCE=${store_resource}`,
      `APELL_KEY_EXCHANGE_RESOURCE=${key_resource}`,
    ].join('\n'),
  );
}

function snapshot() {
  const files = output('git', [
    'ls-files',
    '-z',
    '--cached',
    '--others',
    '--exclude-standard',
    '--',
    'Cargo.toml',
    'Cargo.lock',
    'crates',
    'src-tauri',
    'vendor',
    'store/web/index.html',
    'deploy/local/identity.ts',
  ]);
  if (files.status !== 0) throw new Error('Could not list workspace sources.');
  mkdirSync(workspace, { recursive: true });
  for (const entry of readdirSync(workspace)) rmSync(join(workspace, entry), { recursive: true });
  for (const path of new Set(files.stdout.split('\0').filter(Boolean))) {
    const source = join(root, path);
    if (!existsSync(source) || !lstatSync(source).isFile()) continue;
    const destination = join(workspace, path);
    mkdirSync(dirname(destination), { recursive: true });
    copyFileSync(source, destination);
  }
}

function running() {
  const result = output(engine, [
    'ps',
    '-a',
    '--filter',
    `label=dev.apell.local=${checkout}`,
    '--format',
    '{{.Names}}',
  ]);
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw new Error(`Cannot reach the ${engine} engine.\n${result.stderr.trim()}`);
  return result.stdout.split('\n').includes(name);
}

function start() {
  if (running()) throw new Error('Local services already exist. Use reload or stop first.');
  initialize();
  snapshot();
  run(engine, ['build', '-t', image, local]);
  run(engine, [
    'run',
    '--detach',
    '--rm',
    '--init',
    '--name',
    name,
    '--label',
    `dev.apell.local=${checkout}`,
    '-p',
    '127.0.0.1:5656:5656',
    '-p',
    '127.0.0.1:8789:8789',
    '-v',
    `${workspace}:/workspace:ro`,
    '-v',
    `${config}:/config:ro`,
    '-v',
    `${name}-data:/data`,
    '-v',
    `${name}-target:/target`,
    '-v',
    'apell-local-cargo-registry:/usr/local/cargo/registry',
    '-v',
    'apell-local-cargo-git:/usr/local/cargo/git',
    image,
  ]);
  console.log('Local services are starting. Follow the first Rust build with pnpm dev:local logs.');
  console.log('Run pnpm dev:local web for the Store at https://localhost:8788.');
}

try {
  const command = process.argv[2] ?? 'up';
  switch (command) {
    case 'up':
      start();
      break;
    case 'reload':
      if (running()) run(engine, ['stop', name]);
      start();
      break;
    case 'web':
      initialize();
      run('pnpm', ['store:dev'], {
        ...process.env,
        APELL_DEV_STORE_BACKEND: 'http://127.0.0.1:8789',
        APELL_DEV_TLS_DIR: tls,
      });
      break;
    case 'logs':
      if (!running()) throw new Error('Start the local services first.');
      run(engine, ['logs', '--follow', name]);
      break;
    case 'stop':
      if (running()) run(engine, ['stop', name]);
      break;
    default:
      throw new Error('Usage: pnpm dev:local [up | web | reload | logs | stop]');
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
}

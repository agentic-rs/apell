import assert from 'node:assert/strict';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

type Discovery = {
  issuer: string;
  authorization_endpoint: string;
  token_endpoint: string;
  jwks_uri: string;
  userinfo_endpoint: string;
  response_types_supported: string[];
  code_challenge_methods_supported: string[];
};

const fetchOnce = (url: string | URL, options: RequestInit = {}) =>
  fetch(url, { ...options, redirect: 'manual', signal: AbortSignal.timeout(4000) });

async function discover(endpoint: string) {
  const issuer = `${endpoint}/oidc`;
  const deadline = Date.now() + 90_000;
  while (Date.now() < deadline) {
    const response = await fetchOnce(`${issuer}/.well-known/openid-configuration`).catch(
      () => undefined,
    );
    if (!response?.ok) {
      await Bun.sleep(1000);
      continue;
    }

    const discovery: Discovery = await response.json();
    assert.equal(discovery.issuer, issuer);
    assert.equal(discovery.authorization_endpoint, `${issuer}/auth`);
    assert.equal(discovery.token_endpoint, `${issuer}/token`);
    assert.equal(discovery.jwks_uri, `${issuer}/jwks`);
    assert.equal(discovery.userinfo_endpoint, `${issuer}/me`);
    assert.ok(discovery.response_types_supported.includes('code'));
    assert.ok(discovery.code_challenge_methods_supported.includes('S256'));

    const jwks = await fetchOnce(discovery.jwks_uri);
    assert.equal(jwks.status, 200);
    const { keys }: { keys: Array<JsonWebKey & { kid?: string }> } = await jwks.json();
    assert.ok(keys.some((key) => key.kid && key.kty && !key.d));
    return discovery;
  }
  throw new Error('Logto discovery did not become ready');
}

function authorizationRequest(endpoint: string, discovery: Discovery) {
  const verifier = randomBytes(32).toString('base64url');
  const state = randomUUID();
  const nonce = randomUUID();
  const redirect_uri = `${endpoint}/demo-app`;
  const url = new URL(discovery.authorization_endpoint);
  url.search = new URLSearchParams({
    client_id: 'demo-app',
    redirect_uri,
    response_type: 'code',
    scope: 'openid profile',
    state,
    nonce,
    code_challenge: createHash('sha256').update(verifier).digest('base64url'),
    code_challenge_method: 'S256',
  }).toString();
  return { url, redirect_uri, state };
}

async function checkAuthorization(endpoint: string, discovery: Discovery) {
  const { url, redirect_uri, state } = authorizationRequest(endpoint, discovery);
  const valid = await fetchOnce(url);
  const location = valid.headers.get('location');
  assert.ok(location, 'Logto did not redirect a valid PKCE authorization request');
  const interaction = new URL(location, endpoint);
  assert.equal(interaction.origin, endpoint);
  assert.notEqual(interaction.href, redirect_uri);
  assert.ok(!interaction.searchParams.has('error'));
  assert.ok(valid.headers.getSetCookie().some((cookie) => cookie.startsWith('_interaction=')));

  const missing_pkce = new URL(url);
  missing_pkce.searchParams.delete('code_challenge');
  missing_pkce.searchParams.delete('code_challenge_method');
  const plain_pkce = new URL(url);
  plain_pkce.searchParams.set('code_challenge_method', 'plain');

  for (const invalid of [missing_pkce, plain_pkce]) {
    const denied = await fetchOnce(invalid);
    assert.ok(!denied.headers.getSetCookie().some((cookie) => cookie.startsWith('_interaction=')));
    const denial = denied.headers.get('location');
    if (denial) {
      const error = new URL(denial, endpoint);
      assert.equal(error.origin + error.pathname, redirect_uri);
      assert.equal(error.searchParams.get('error'), 'invalid_request');
      assert.equal(error.searchParams.get('state'), state);
      assert.ok(!error.searchParams.has('code'));
    } else {
      assert.equal(denied.status, 400);
    }
  }

  const unexpected_callback = new URL(url);
  unexpected_callback.searchParams.set('redirect_uri', 'https://unregistered.example.invalid');
  const wrong_callback = await fetchOnce(unexpected_callback);
  assert.equal(wrong_callback.status, 400);
  assert.equal(wrong_callback.headers.get('location'), null);
}

function runCompose(compose: string[], args: string[], env: NodeJS.ProcessEnv) {
  const result = Bun.spawnSync([...compose, ...args], { env, stdout: 'pipe', stderr: 'pipe' });
  if (result.exitCode !== 0) {
    throw new Error(`docker compose ${args[0]} failed with exit code ${result.exitCode}`);
  }
}

async function checkDisposableStack() {
  const reservations = Array.from({ length: 2 }, () =>
    Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: () => new Response(null) }),
  );
  const [account_port, admin_port] = reservations.map((server) => server.port);
  const endpoint = `http://127.0.0.1:${account_port}`;
  const folder = await mkdtemp(join(tmpdir(), 'apell-account-smoke-'));
  const env_file = join(folder, 'fixture.env');
  const compose = [
    'docker',
    'compose',
    '--project-name',
    `apell-account-smoke-${randomUUID().slice(0, 8)}`,
    '--env-file',
    env_file,
    '--file',
    join(import.meta.dir, 'compose.yaml'),
  ];
  const env = {
    ...process.env,
    ACCOUNT_ENDPOINT: endpoint,
    ACCOUNT_PORT: String(account_port),
    ACCOUNT_ADMIN_PORT: String(admin_port),
    ACCOUNT_DB_PASSWORD: randomBytes(32).toString('hex'),
  };

  let started = false;
  try {
    // An explicit empty env file keeps the disposable project independent of operator credentials.
    await Bun.write(env_file, '');
    await Promise.all(reservations.map((server) => server.stop(true)));
    started = true;
    runCompose(compose, ['up', '-d'], env);
    await checkAuthorization(endpoint, await discover(endpoint));
    console.log('Disposable Logto smoke passed (discovery, JWKS, and S256 PKCE).');
  } finally {
    try {
      if (started) runCompose(compose, ['down', '--volumes', '--remove-orphans'], env);
    } finally {
      if (!started) await Promise.all(reservations.map((server) => server.stop(true)));
      await rm(folder, { recursive: true, force: true });
    }
  }
}

const [option, argument, ...extra] = process.argv.slice(2);
if (!option) {
  await checkDisposableStack();
} else if (option === '--endpoint' && argument && !extra.length) {
  const endpoint = new URL(argument);
  assert.equal(endpoint.href, `${endpoint.origin}/`, '--endpoint must be the Logto origin');
  assert.ok(
    endpoint.protocol === 'https:' ||
      (endpoint.protocol === 'http:' && ['127.0.0.1', 'localhost'].includes(endpoint.hostname)),
    'Use a public HTTPS endpoint or a loopback HTTP endpoint',
  );
  await discover(endpoint.origin);
  console.log('Logto discovery and public signing keys passed.');
} else {
  throw new Error('Usage: bun deploy/account/smoke.ts [--endpoint https://auth.example.invalid]');
}

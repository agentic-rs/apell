import assert from 'node:assert/strict';
import { chmod, mkdtemp, realpath, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const client_id = 'apell-store-web';
const redirect_uri = 'https://store.example.invalid/api/v1/auth/account/callback';
const upstream_origin = 'https://upstream:6443';
const github_user = { id: 514928, login: 'fixture-publisher', email: 'publisher@example.invalid' };

function randomSecret() {
  return Buffer.from(crypto.getRandomValues(new Uint8Array(32))).toString('base64url');
}

function run(command: string[], env = process.env) {
  const result = Bun.spawnSync(command, { env, stdout: 'pipe', stderr: 'pipe' });
  if (result.exitCode !== 0) {
    throw new Error(`${command.slice(0, 2).join(' ')} failed: ${result.stderr.toString().trim()}`);
  }
  return result.stdout.toString().trim();
}

function serveGithub() {
  const upstream_client = process.env.ACCOUNT_GITHUB_CLIENT_ID;
  const upstream_secret = process.env.ACCOUNT_GITHUB_CLIENT_SECRET;
  const callback_uri = `${process.env.ACCOUNT_ISSUER}/callback`;
  const codes = new Set<string>();
  const access_token = randomSecret();

  Bun.serve({
    hostname: '0.0.0.0',
    port: 6443,
    tls: { cert: Bun.file('/fixture/cert.pem'), key: Bun.file('/fixture/key.pem') },
    async fetch(request) {
      const { pathname, searchParams } = new URL(request.url);
      if (request.method === 'GET' && pathname === '/login/oauth/authorize') {
        const state = searchParams.get('state');
        if (
          searchParams.get('client_id') !== upstream_client ||
          searchParams.get('redirect_uri') !== callback_uri ||
          !state
        ) {
          return new Response('Invalid fixture authorization request', { status: 400 });
        }
        const code = randomSecret();
        codes.add(code);
        const redirect = new URL(callback_uri);
        redirect.search = new URLSearchParams({ code, state }).toString();
        return Response.redirect(redirect, 302);
      }
      if (request.method === 'POST' && pathname === '/login/oauth/access_token') {
        const form = new URLSearchParams(await request.text());
        const basic = `Basic ${Buffer.from(`${upstream_client}:${upstream_secret}`).toString('base64')}`;
        const authenticated =
          request.headers.get('authorization') === basic ||
          (form.get('client_id') === upstream_client &&
            form.get('client_secret') === upstream_secret);
        if (
          !authenticated ||
          form.get('redirect_uri') !== callback_uri ||
          !codes.delete(form.get('code') ?? '')
        ) {
          return Response.json({ error: 'invalid_grant' }, { status: 400 });
        }
        return Response.json({ access_token, token_type: 'bearer', scope: 'user:email' });
      }
      if (request.method === 'GET' && pathname === '/api/v3/user') {
        if (request.headers.get('authorization') !== `Bearer ${access_token}`) {
          return new Response(null, { status: 401 });
        }
        return Response.json(github_user);
      }
      return new Response(null, { status: 404 });
    },
  });
}

async function discoveryAt(issuer: string) {
  const deadline = Date.now() + 45_000;
  while (Date.now() < deadline) {
    const response = await fetch(`${issuer}/.well-known/openid-configuration`, {
      signal: AbortSignal.timeout(2000),
    }).catch(() => undefined);
    if (response?.ok) return response.json();
    await Bun.sleep(500);
  }
  throw new Error('Account discovery did not become ready');
}

async function checkAccount(
  issuer: string,
  upstream_proxy: string,
  cert: string,
  client_secret: string,
) {
  const discovery = await discoveryAt(issuer);
  assert.equal(discovery.issuer, issuer);
  assert.equal(discovery.authorization_endpoint, `${issuer}/auth`);
  assert.equal(discovery.token_endpoint, `${issuer}/token`);
  assert.equal(discovery.jwks_uri, `${issuer}/keys`);
  assert.deepEqual(discovery.response_types_supported, ['code']);
  assert.deepEqual(discovery.grant_types_supported, ['authorization_code']);
  assert.deepEqual(discovery.code_challenge_methods_supported, ['S256']);

  const verifier = randomSecret();
  const challenge = Buffer.from(
    await crypto.subtle.digest('SHA-256', Buffer.from(verifier)),
  ).toString('base64url');
  const state = randomSecret();
  const nonce = randomSecret();
  const common = {
    client_id,
    redirect_uri,
    response_type: 'code',
    scope: 'openid profile federated:id',
    state,
    nonce,
  };

  const authorizationUrl = (params: Record<string, string>) =>
    `${discovery.authorization_endpoint}?${new URLSearchParams({ ...common, ...params })}`;
  const s256 = { code_challenge: challenge, code_challenge_method: 'S256' };

  for (const params of [
    {},
    { code_challenge: verifier, code_challenge_method: 'plain' },
    { ...s256, redirect_uri: `${redirect_uri}/unexpected` },
  ]) {
    let address = new URL(authorizationUrl(params));
    for (let step = 0; ; step += 1) {
      assert.ok(step < 4, 'Rejected authorization exceeded the redirect limit');
      const denied = await fetch(address, { redirect: 'manual' });
      const location = denied.headers.get('location');
      if (!location) {
        assert.equal(denied.status, 400);
        break;
      }
      address = new URL(location, address);
      if (address.origin !== issuer) {
        assert.equal(params.redirect_uri, undefined, 'Unregistered callback was accepted');
        assert.equal(`${address.origin}${address.pathname}`, redirect_uri);
        assert.equal(address.searchParams.get('error'), 'invalid_request');
        assert.equal(address.searchParams.get('state'), state);
        break;
      }
    }
  }

  async function getCode() {
    const cookies = new Map<string, string>();
    let url = new URL(authorizationUrl(s256));
    let visited_upstream = false;
    let consent: URLSearchParams | undefined;
    for (let step = 0; step < 12; step += 1) {
      if (`${url.origin}${url.pathname}` === redirect_uri) {
        assert.ok(visited_upstream, 'GitHub connector was skipped');
        assert.equal(url.searchParams.get('state'), state);
        const code = url.searchParams.get('code');
        assert.ok(code, `Missing authorization code: ${url.searchParams.get('error')}`);
        return code;
      }

      const upstream = url.origin === upstream_origin;
      assert.ok(upstream || url.origin === issuer, 'Unexpected sign-in redirect origin');
      const address = upstream ? new URL(`${url.pathname}${url.search}`, upstream_proxy) : url;
      visited_upstream ||= upstream;
      const response = await fetch(address, {
        redirect: 'manual',
        signal: AbortSignal.timeout(10_000),
        ...(consent && { method: 'POST', body: consent }),
        ...(upstream
          ? { tls: { ca: cert } }
          : { headers: { cookie: Array.from(cookies.values()).join('; ') } }),
      }).catch((cause) => {
        throw new Error(`Could not reach sign-in path ${url.pathname}`, { cause });
      });
      if (!upstream) {
        for (const header of response.headers.getSetCookie()) {
          const cookie = header.split(';', 1)[0];
          cookies.set(cookie.split('=', 1)[0], cookie);
        }
      }
      const location = response.headers.get('location');
      if (!location && !consent && url.pathname === '/approval' && response.ok) {
        const html = await response.text();
        const req = html.match(/name="req" value="([a-zA-Z0-9_-]+)"/)?.[1];
        assert.ok(req, 'Missing Dex consent form request identifier');
        consent = new URLSearchParams({ req, approval: 'approve' });
        continue;
      }
      assert.ok(location, `Sign-in stalled at ${url.pathname} with status ${response.status}`);
      consent = undefined;
      url = new URL(location, url);
    }
    throw new Error('Sign-in exceeded the redirect limit');
  }

  const exchangeCode = (code: string, code_verifier: string) =>
    fetch(discovery.token_endpoint, {
      redirect: 'error',
      headers: {
        authorization: `Basic ${Buffer.from(`${client_id}:${client_secret}`).toString('base64')}`,
      },
      body: new URLSearchParams({
        grant_type: 'authorization_code',
        code,
        redirect_uri,
        code_verifier,
      }),
      method: 'POST',
    });

  const wrong_verifier = await exchangeCode(await getCode(), randomSecret());
  assert.ok(!wrong_verifier.ok, 'Token endpoint accepted an incorrect PKCE verifier');

  const code = await getCode();
  const token_response = await exchangeCode(code, verifier);
  const tokens = await token_response.json();
  assert.ok(token_response.ok, `Token exchange failed: ${tokens.error}`);
  assert.equal(tokens.refresh_token, undefined);
  assert.ok(tokens.access_token);
  assert.equal(typeof tokens.id_token, 'string');

  const jwt = tokens.id_token.split('.');
  assert.equal(jwt.length, 3);
  const [header, payload, signature] = jwt;
  const jwt_header = JSON.parse(Buffer.from(header, 'base64url').toString());
  const claims = JSON.parse(Buffer.from(payload, 'base64url').toString());
  const jwks_response = await fetch(discovery.jwks_uri);
  assert.ok(jwks_response.ok);
  const { keys } = await jwks_response.json();
  assert.equal(jwt_header.alg, 'RS256');
  const jwk = keys.find((key: JsonWebKey & { kid: string }) => key.kid === jwt_header.kid);
  assert.ok(jwk, 'ID token signing key missing from JWKS');
  const algorithm = { name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' };
  const public_key = await crypto.subtle.importKey('jwk', jwk, algorithm, false, ['verify']);
  const valid = await crypto.subtle.verify(
    algorithm,
    public_key,
    Buffer.from(signature, 'base64url'),
    Buffer.from(`${header}.${payload}`),
  );
  assert.ok(valid, 'Invalid ID token signature');
  assert.equal(claims.iss, issuer);
  assert.deepEqual([claims.aud].flat(), [client_id]);
  assert.ok(claims.exp > Date.now() / 1000);
  assert.equal(claims.nonce, nonce);
  assert.ok(claims.sub);
  assert.equal(claims.email, undefined);
  assert.equal(claims.preferred_username, github_user.login);
  assert.deepEqual(claims.federated_claims, {
    connector_id: 'github',
    user_id: String(github_user.id),
  });

  const replay = await exchangeCode(code, verifier);
  assert.ok(!replay.ok, 'Token endpoint accepted the same authorization code twice');
}

async function smoke() {
  const listening_ports = Array.from({ length: 2 }, () =>
    Bun.serve({ hostname: '127.0.0.1', port: 0, fetch: () => new Response(null) }),
  );
  const [account_port, upstream_port] = listening_ports.map((server) => server.port);
  const issuer = `http://127.0.0.1:${account_port}`;
  const upstream_proxy = `https://127.0.0.1:${upstream_port}`;
  const folder = await mkdtemp(join(await realpath(tmpdir()), 'apell-account-smoke-'));
  const cert_file = join(folder, 'cert.pem');
  const key_file = join(folder, 'key.pem');
  const template_file = join(folder, 'dex.yaml.tmpl');
  const override_file = join(folder, 'compose.override.json');
  const compose_args = [
    'docker',
    'compose',
    '--project-name',
    `apell-account-smoke-${crypto.randomUUID().slice(0, 8)}`,
    '--file',
    join(import.meta.dir, 'compose.yaml'),
    '--file',
    override_file,
  ];
  const env = {
    ...process.env,
    ACCOUNT_ISSUER: issuer,
    ACCOUNT_PORT: String(account_port),
    ACCOUNT_DB_PASSWORD: randomSecret(),
    ACCOUNT_SESSION_KEY: randomSecret().slice(0, 32),
    ACCOUNT_GITHUB_CLIENT_ID: 'fixture-github-client',
    ACCOUNT_GITHUB_CLIENT_SECRET: randomSecret(),
    ACCOUNT_STORE_REDIRECT_URI: redirect_uri,
    ACCOUNT_STORE_CLIENT_SECRET: randomSecret(),
  };

  let started = false;
  try {
    run([
      'openssl',
      'req',
      '-x509',
      '-newkey',
      'rsa:2048',
      '-nodes',
      '-days',
      '1',
      '-subj',
      '/CN=upstream',
      '-addext',
      'subjectAltName=DNS:upstream,IP:127.0.0.1',
      '-keyout',
      key_file,
      '-out',
      cert_file,
    ]);
    await chmod(cert_file, 0o644);
    await chmod(key_file, 0o644);

    const template = await Bun.file(join(import.meta.dir, 'dex.yaml.tmpl')).text();
    const connector_setting = /^([ ]+)useLoginAsID: false$/m;
    assert.match(
      template,
      connector_setting,
      'Synthetic provider needs the GitHub connector settings',
    );
    await Bun.write(
      template_file,
      template.replace(
        connector_setting,
        '$&\n$1hostName: upstream:6443\n$1rootCA: /etc/dex/upstream.pem',
      ),
    );
    await chmod(template_file, 0o644);
    await Bun.write(
      override_file,
      JSON.stringify({
        services: {
          account: {
            environment: { NO_PROXY: 'upstream', no_proxy: 'upstream' },
            volumes: [
              `${template_file}:/etc/dex/account.yaml.tmpl:ro`,
              `${cert_file}:/etc/dex/upstream.pem:ro`,
            ],
          },
          upstream: {
            image: 'docker.io/oven/bun:1.3.13-alpine',
            command: ['bun', '/fixture/upstream.ts', '--synthetic-github'],
            networks: ['frontend'],
            ports: [`127.0.0.1:${upstream_port}:6443`],
            environment: [
              'ACCOUNT_ISSUER',
              'ACCOUNT_GITHUB_CLIENT_ID',
              'ACCOUNT_GITHUB_CLIENT_SECRET',
            ],
            volumes: [
              `${join(import.meta.dir, 'smoke.ts')}:/fixture/upstream.ts:ro`,
              `${cert_file}:/fixture/cert.pem:ro`,
              `${key_file}:/fixture/key.pem:ro`,
            ],
          },
        },
      }),
    );

    await Promise.all(listening_ports.map((server) => server.stop(true)));
    started = true;
    run([...compose_args, 'up', '-d'], env);
    await checkAccount(
      issuer,
      upstream_proxy,
      await Bun.file(cert_file).text(),
      env.ACCOUNT_STORE_CLIENT_SECRET,
    );
    console.log(
      'Account smoke passed with the production Compose configuration and synthetic GitHub.',
    );
  } catch (error) {
    if (started) {
      for (const service of ['account', 'upstream']) {
        Bun.spawnSync([...compose_args, 'logs', '--tail', '25', service], {
          env,
          stdout: 'inherit',
          stderr: 'inherit',
        });
      }
    }
    throw error;
  } finally {
    try {
      if (started) run([...compose_args, 'down', '--volumes', '--remove-orphans'], env);
    } finally {
      if (!started) await Promise.all(listening_ports.map((server) => server.stop(true)));
      await rm(folder, { recursive: true, force: true });
    }
  }
}

if (process.argv[2] === '--synthetic-github') {
  serveGithub();
} else {
  await smoke();
}

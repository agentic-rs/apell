// Synthetic identity provider for the loopback Docker stack. No real accounts.
import { Database } from 'bun:sqlite';
import {
  createHash,
  createPrivateKey,
  createPublicKey,
  randomBytes,
  sign,
  timingSafeEqual,
} from 'node:crypto';
import { join } from 'node:path';

const account_origin = 'https://localhost:5656';
const store_origin = 'https://localhost:8788';
const store_resource = `${store_origin}/api/v1/config-sync`;
const issuer = `${account_origin}/oidc`;
const relay_path = '/api/v1/key-vault';
const relay_resource = `${account_origin}${relay_path}`;
const shell_client = 'apell-local-shell';
const device_grant = 'urn:ietf:params:oauth:grant-type:device_code';
const recovery_acr = 'urn:logto:acr:1fa';
const subject = 'apell-local-user';
const email = 'developer@example.invalid';
const github_id = Number.MAX_SAFE_INTEGER;
const max_grants = 256;
const refresh_lifetime = 86_400;
const now = () => Math.floor(Date.now() / 1000);
const randomToken = () => randomBytes(32).toString('base64url');
const tokenDigest = (value: string) => createHash('sha256').update(value).digest('hex');

const config_dir = process.env.APELL_DEV_CONFIG_DIR ?? '/config';
const tls_dir = process.env.APELL_DEV_TLS_DIR ?? '/config';
const data_dir = process.env.APELL_DEV_DATA_DIR ?? '/data';

async function readSigningKey() {
  try {
    const key = createPrivateKey(await Bun.file(join(config_dir, 'identity-key.pem')).text());
    if (key.asymmetricKeyType !== 'rsa' || key.asymmetricKeyDetails?.modulusLength !== 2048) {
      throw new Error();
    }
    return key;
  } catch {
    throw new Error('Missing or invalid local identity configuration in identity-key.pem.');
  }
}

const signing_key = await readSigningKey();
const public_key = createPublicKey(signing_key);
const kid = createHash('sha256')
  .update(public_key.export({ format: 'der', type: 'spki' }))
  .digest('base64url');
const jwks = { keys: [{ ...public_key.export({ format: 'jwk' }), kid, alg: 'RS256', use: 'sig' }] };

async function webClient(filename: string, client_id: string, redirect_uri: string) {
  try {
    const file = await Bun.file(join(config_dir, filename)).json();
    if (
      file.client_id !== client_id ||
      typeof file.client_secret !== 'string' ||
      !/^[!-~]{16,2048}$/.test(file.client_secret)
    ) {
      throw new Error();
    }
    const encode = (value: string) => new URLSearchParams([['', value]]).toString().slice(1);
    const basic = Buffer.from(`${encode(client_id)}:${encode(file.client_secret)}`).toString(
      'base64',
    );
    return { client_id, redirect_uri, authorization: Buffer.from(`Basic ${basic}`) };
  } catch {
    throw new Error(`Missing or invalid local identity configuration in ${filename}.`);
  }
}

const store_client = await webClient(
  'store-oidc.json',
  'apell-local-store',
  `${store_origin}/api/v1/auth/account/callback`,
);
const recovery_client = await webClient(
  'recovery-oidc.json',
  'apell-local-recovery',
  `${relay_resource}/managed/recovery/callback`,
);
const clients = [store_client, recovery_client];

const discovery = {
  issuer,
  authorization_endpoint: `${issuer}/auth`,
  token_endpoint: `${issuer}/token`,
  jwks_uri: `${issuer}/jwks`,
  userinfo_endpoint: `${issuer}/me`,
  device_authorization_endpoint: `${issuer}/device-auth`,
  revocation_endpoint: `${issuer}/revoke`,
};

const resources = new Map([
  [store_resource, ['config:read', 'config:write', 'credentials:read', 'credentials:write']],
  [relay_resource, ['key_exchange:read', 'key_exchange:write']],
]);

type Expiring = { expires_at: number };
type WebRequest = Expiring & {
  client: typeof store_client;
  state: string;
  nonce: string;
  challenge: string;
};
type AuthorizedWebRequest = WebRequest & { auth_time: number };
type DeviceAccess = Expiring & { scopes: Map<string, string> };
type DeviceRequest = DeviceAccess & { user_code: string; approved?: boolean };

const web_requests = new Map<string, WebRequest>();
const web_codes = new Map<string, AuthorizedWebRequest>();
const web_tokens = new Map<string, Expiring>();
const device_requests = new Map<string, DeviceRequest>();

// Completed native sessions survive container reloads. Bearer secrets stay with the device.
const sessions = new Database(join(data_dir, 'identity.sqlite3'));
sessions.exec(`
  CREATE TABLE IF NOT EXISTS refresh_sessions (
    token_hash TEXT PRIMARY KEY,
    store_scope TEXT NOT NULL,
    relay_scope TEXT,
    expires_at INTEGER NOT NULL
  ) STRICT
`);
const prune_sessions = sessions.query('DELETE FROM refresh_sessions WHERE expires_at <= ?');
const insert_session = sessions.query(`
  INSERT INTO refresh_sessions (token_hash, store_scope, relay_scope, expires_at)
  SELECT ?, ?, ?, ? WHERE (SELECT COUNT(*) FROM refresh_sessions) < ?
`);
const find_session = sessions.query<
  { store_scope: string; relay_scope: string | null },
  [string, number]
>(`
  SELECT store_scope, relay_scope FROM refresh_sessions
  WHERE token_hash = ? AND expires_at > ?
`);
const revoke_session = sessions.query('DELETE FROM refresh_sessions WHERE token_hash = ?');
const saveSession = sessions.transaction((refresh_token: string, scopes: Map<string, string>) => {
  const time = now();
  prune_sessions.run(time);
  return (
    insert_session.run(
      tokenDigest(refresh_token),
      scopes.get(store_resource) ?? '',
      scopes.get(relay_resource) ?? null,
      time + refresh_lifetime,
      max_grants,
    ).changes === 1
  );
});
prune_sessions.run(now());

function sessionScope(audience: string, refresh_token: string) {
  const session = find_session.get(tokenDigest(refresh_token), now());
  if (audience === store_resource) return session?.store_scope;
  if (audience === relay_resource) return session?.relay_scope;
}

function remember<T extends Expiring>(entries: Map<string, T>, key: string, value: T) {
  const time = now();
  for (const [old_key, entry] of entries) {
    if (entry.expires_at <= time) entries.delete(old_key);
  }
  if (entries.size >= max_grants) return false;
  entries.set(key, value);
  return true;
}

function active<T extends Expiring>(entries: Map<string, T>, key: string | null) {
  const entry = key === null ? undefined : entries.get(key);
  return entry && entry.expires_at > now() ? entry : undefined;
}

function json(body: unknown, status = 200) {
  return Response.json(body, { status, headers: { 'cache-control': 'no-store' } });
}

function oauthError(error: string, status = 400) {
  return json({ error }, status);
}

function page(heading: string, body: string, status = 200) {
  return new Response(
    `<!doctype html><html lang="en"><meta charset="utf-8">
    <meta name="viewport" content="width=device-width,initial-scale=1">
    <title>${heading} · Apell Local Account</title>
    <main><h1>${heading}</h1><p>Local development · ${email}</p>${body}</main></html>`,
    {
      status,
      headers: {
        'cache-control': 'no-store',
        'content-type': 'text/html; charset=utf-8',
        'content-security-policy': `default-src 'none'; form-action 'self' ${store_origin}; frame-ancestors 'none'`,
        'referrer-policy': 'same-origin',
        'x-content-type-options': 'nosniff',
      },
    },
  );
}

function approvalForm(path: string, name: string, value: string) {
  return `<form action="${path}" method="post">
    <input type="hidden" name="${name}" value="${value}">
    <button name="decision" value="approve">Approve</button>
    <button name="decision" value="deny">Deny</button>
    </form>`;
}

async function readForm(request: Request) {
  return request.headers.get('content-type')?.split(';', 1)[0] ===
    'application/x-www-form-urlencoded'
    ? new URLSearchParams(await request.text())
    : null;
}

async function browserForm(request: Request) {
  if (request.headers.get('origin') !== account_origin) return null;
  return readForm(request);
}

function jwt(audience: string, expires_in: number, extra: Record<string, unknown> = {}) {
  const iat = now();
  const encode = (value: unknown) => Buffer.from(JSON.stringify(value)).toString('base64url');
  const header = encode({ alg: 'RS256', kid, typ: 'JWT' });
  const payload = encode({
    iss: issuer,
    sub: subject,
    aud: audience,
    iat,
    exp: iat + expires_in,
    ...extra,
  });
  const input = `${header}.${payload}`;
  return `${input}.${sign('sha256', Buffer.from(input), signing_key).toString('base64url')}`;
}

function startWeb(params: URLSearchParams) {
  const client = clients.find(({ client_id }) => client_id === params.get('client_id'));
  const state = params.get('state') ?? '';
  const nonce = params.get('nonce') ?? '';
  const challenge = params.get('code_challenge') ?? '';
  const recovery = client === recovery_client;
  if (
    !client ||
    params.get('redirect_uri') !== client.redirect_uri ||
    params.get('response_type') !== 'code' ||
    params.get('scope') !== (recovery ? 'openid' : 'openid identities') ||
    params.get('code_challenge_method') !== 'S256' ||
    !/^[\w-]{43}$/.test(challenge) ||
    !/^[\w-]{1,128}$/.test(state) ||
    !/^[\w-]{1,128}$/.test(nonce) ||
    (recovery &&
      (params.get('prompt') !== 'login' ||
        params.get('max_age') !== '0' ||
        params.get('acr_values') !== recovery_acr))
  ) {
    return page('Invalid authorization request', '', 400);
  }
  const ticket = randomToken();
  if (
    !remember(web_requests, ticket, { client, state, nonce, challenge, expires_at: now() + 300 })
  ) {
    return page('Too many pending requests', '', 503);
  }
  return page(
    recovery ? 'Approve vault verification' : 'Approve Store sign-in',
    approvalForm('/oidc/auth', 'ticket', ticket),
  );
}

async function completeWeb(request: Request) {
  const form = await browserForm(request);
  const ticket = form?.get('ticket') ?? '';
  const pending = active(web_requests, ticket);
  const decision = form?.get('decision');
  if (!pending || (decision !== 'approve' && decision !== 'deny')) {
    return page('Invalid or expired approval', '', 400);
  }
  web_requests.delete(ticket);
  const callback = new URL(pending.client.redirect_uri);
  callback.searchParams.set('state', pending.state);
  if (decision === 'approve') {
    const code = randomToken();
    if (!remember(web_codes, code, { ...pending, auth_time: now(), expires_at: now() + 60 })) {
      return page('Too many pending requests', '', 503);
    }
    callback.searchParams.set('code', code);
  } else {
    callback.searchParams.set('error', 'access_denied');
  }
  return new Response(null, {
    status: 303,
    headers: {
      location: callback.href,
      'cache-control': 'no-store',
      'referrer-policy': 'no-referrer',
    },
  });
}

function exchangeWeb(request: Request, form: URLSearchParams) {
  const code = form.get('code') ?? '';
  const grant = active(web_codes, code);
  if (!grant || form.get('redirect_uri') !== grant.client.redirect_uri) {
    return oauthError('invalid_grant');
  }
  const given = Buffer.from(request.headers.get('authorization') ?? '');
  if (
    given.length !== grant.client.authorization.length ||
    !timingSafeEqual(given, grant.client.authorization)
  ) {
    return oauthError('invalid_client', 401);
  }
  const verifier = form.get('code_verifier') ?? '';
  if (
    !/^[A-Za-z0-9._~-]{43,128}$/.test(verifier) ||
    createHash('sha256').update(verifier).digest('base64url') !== grant.challenge
  ) {
    return oauthError('invalid_grant');
  }
  web_codes.delete(code);
  const access_token = randomToken();
  const expires_in = 300;
  if (!remember(web_tokens, access_token, { expires_at: now() + expires_in })) {
    return oauthError('temporarily_unavailable', 503);
  }
  return json({
    access_token,
    token_type: 'Bearer',
    expires_in,
    id_token: jwt(grant.client.client_id, expires_in, {
      nonce: grant.nonce,
      auth_time: grant.auth_time,
      acr: recovery_acr,
    }),
  });
}

function userInfo(request: Request) {
  const authorization = request.headers.get('authorization') ?? '';
  if (!authorization.startsWith('Bearer ') || !active(web_tokens, authorization.slice(7))) {
    return oauthError('invalid_token', 401);
  }
  return json({
    sub: subject,
    email,
    email_verified: true,
    identities: {
      github: {
        userId: String(github_id),
        details: { rawData: { userInfo: { id: github_id, login: subject } } },
      },
    },
  });
}

async function startDevice(request: Request) {
  const form = await readForm(request);
  if (form?.get('client_id') !== shell_client) return oauthError('invalid_client', 401);
  const audiences = form.getAll('resource');
  const scope = (form.get('scope') ?? '').split(' ');
  if (
    !scope.includes('openid') ||
    !scope.includes('offline_access') ||
    !audiences.includes(store_resource) ||
    audiences.some((resource) => !resources.has(resource))
  ) {
    return oauthError('invalid_request');
  }
  const scopes = new Map(
    audiences.map((audience) => [
      audience,
      resources
        .get(audience)
        ?.filter((value) => scope.includes(value))
        .join(' ') ?? '',
    ]),
  );
  const device_code = randomToken();
  const code = randomBytes(5).toString('hex').toUpperCase();
  const user_code = `${code.slice(0, 5)}-${code.slice(5)}`;
  const expires_in = 300;
  if (
    !remember(device_requests, device_code, { user_code, scopes, expires_at: now() + expires_in })
  ) {
    return oauthError('temporarily_unavailable', 503);
  }
  const verification_uri = `${account_origin}/activate`;
  return json({
    device_code,
    user_code,
    verification_uri,
    verification_uri_complete: `${verification_uri}?user_code=${user_code}`,
    expires_in,
    interval: 5,
  });
}

async function activate(request: Request, params: URLSearchParams) {
  const form = request.method === 'POST' ? await browserForm(request) : params;
  const user_code = form?.get('user_code')?.trim().toUpperCase();
  if (request.method === 'GET' && !user_code) {
    return page(
      'Enter your device code',
      '<form action="/activate"><label>Device code <input name="user_code" required></label> <button>Continue</button></form>',
    );
  }
  const pending = [...device_requests.values()].find(
    (entry) =>
      entry.user_code === user_code && entry.expires_at > now() && entry.approved === undefined,
  );
  if (!pending) return page('Invalid or expired device code', '', 400);
  if (request.method === 'GET') {
    return page(
      'Approve device sign-in',
      `<p>Check that your device shows <strong>${pending.user_code}</strong>.</p>` +
        approvalForm('/activate', 'user_code', pending.user_code),
    );
  }
  const decision = form?.get('decision');
  if (decision !== 'approve' && decision !== 'deny') return page('Invalid approval', '', 400);
  pending.approved = decision === 'approve';
  return page(
    pending.approved ? 'Device approved' : 'Device sign-in denied',
    '<p>Return to Apell.</p>',
  );
}

function shellTokens(audience: string, scope: string, refresh_token: string) {
  const expires_in = 300;
  return json({
    access_token: jwt(audience, expires_in, { client_id: shell_client, scope }),
    id_token: jwt(shell_client, expires_in, { email, email_verified: true }),
    refresh_token,
    token_type: 'Bearer',
    expires_in,
  });
}

async function token(request: Request) {
  const form = await readForm(request);
  if (!form) return oauthError('invalid_request');
  const grant_type = form.get('grant_type');
  if (grant_type === 'authorization_code') return exchangeWeb(request, form);
  if (form.get('client_id') !== shell_client) return oauthError('invalid_client', 401);
  const audience = form.get('resource') ?? '';
  if (grant_type === 'refresh_token') {
    const refresh_token = form.get('refresh_token') ?? '';
    const scope = sessionScope(audience, refresh_token);
    return scope == null
      ? oauthError('invalid_grant')
      : shellTokens(audience, scope, refresh_token);
  }
  if (grant_type !== device_grant) return oauthError('unsupported_grant_type');
  const device_code = form.get('device_code') ?? '';
  const pending = active(device_requests, device_code);
  if (!pending) return oauthError('expired_token');
  const scope = pending.scopes.get(audience);
  if (scope === undefined) return oauthError('invalid_target');
  if (pending.approved === undefined) return oauthError('authorization_pending');
  device_requests.delete(device_code);
  if (!pending.approved) return oauthError('access_denied');
  const refresh_token = randomToken();
  if (!saveSession(refresh_token, pending.scopes)) {
    return oauthError('temporarily_unavailable', 503);
  }
  return shellTokens(audience, scope, refresh_token);
}

async function revoke(request: Request) {
  const form = await readForm(request);
  if (form?.get('client_id') !== shell_client) return oauthError('invalid_client', 401);
  revoke_session.run(tokenDigest(form.get('token') ?? ''));
  return new Response(null, { headers: { 'cache-control': 'no-store' } });
}

async function proxyRelay(request: Request, url: URL) {
  const headers = new Headers();
  for (const name of ['accept', 'authorization', 'content-type']) {
    const value = request.headers.get(name);
    if (value !== null) headers.set(name, value);
  }
  const has_body = request.method !== 'GET' && request.method !== 'HEAD';
  const target = new URL('http://127.0.0.1:5657');
  target.pathname = url.pathname;
  target.search = url.search;
  try {
    return await fetch(target, {
      method: request.method,
      headers,
      body: has_body ? await request.arrayBuffer() : undefined,
      redirect: 'manual',
      signal: AbortSignal.timeout(15_000),
    });
  } catch {
    return oauthError('relay_unavailable', 502);
  }
}

Bun.serve({
  hostname: '0.0.0.0',
  port: 5656,
  tls: {
    cert: Bun.file(join(tls_dir, 'localhost.crt')),
    key: Bun.file(join(tls_dir, 'localhost.key')),
  },
  maxRequestBodySize: 16_384,
  fetch(request) {
    const url = new URL(request.url);
    const { method } = request;
    if (url.host !== new URL(account_origin).host) return oauthError('invalid_host', 400);
    if (url.pathname === relay_path || url.pathname.startsWith(`${relay_path}/`)) {
      return proxyRelay(request, url);
    }
    switch (`${method} ${url.pathname}`) {
      case 'GET /oidc/.well-known/openid-configuration':
        return json(discovery);
      case 'GET /oidc/jwks':
        return json(jwks);
      case 'GET /oidc/auth':
        return startWeb(url.searchParams);
      case 'POST /oidc/auth':
        return completeWeb(request);
      case 'POST /oidc/token':
        return token(request);
      case 'GET /oidc/me':
        return userInfo(request);
      case 'POST /oidc/device-auth':
        return startDevice(request);
      case 'GET /activate':
      case 'POST /activate':
        return activate(request, url.searchParams);
      case 'POST /oidc/revoke':
        return revoke(request);
      case 'GET /account/security':
        return page(
          'Local Account',
          '<p>This fixture uses a fictional identity and a linked test profile.</p>',
        );
      default:
        return new Response(null, { status: 404 });
    }
  },
  error() {
    return oauthError('local_identity_error', 500);
  },
});

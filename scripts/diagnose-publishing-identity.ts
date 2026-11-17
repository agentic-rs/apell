// Temporary CI diagnostics: emit only fixed-name booleans, never token/claim values.
async function diagnose() {
  const audience = new URL(process.env.APELL_STORE_URL!).origin;
  const endpoint = new URL(process.env.ACTIONS_ID_TOKEN_REQUEST_URL!);
  if (
    endpoint.protocol !== 'https:' ||
    !endpoint.hostname.endsWith('.actions.githubusercontent.com') ||
    endpoint.username ||
    endpoint.password ||
    endpoint.hash
  )
    throw new Error('Invalid identity endpoint.');
  endpoint.searchParams.set('audience', audience);
  const response = await fetch(endpoint, {
    headers: { authorization: `Bearer ${process.env.ACTIONS_ID_TOKEN_REQUEST_TOKEN!}` },
    redirect: 'error',
    signal: AbortSignal.timeout(15000),
  });
  if (!response.ok) throw new Error('Identity request failed.');
  const { value } = (await response.json()) as { value: string };
  if (value.length > 16384) throw new Error('Identity too large.');
  const [encoded_header, encoded_claims] = value.split('.');
  const header = JSON.parse(Buffer.from(encoded_header, 'base64url').toString('utf8'));
  const claims = JSON.parse(Buffer.from(encoded_claims, 'base64url').toString('utf8'));
  const now = Math.floor(Date.now() / 1000);
  const expected_workflow = `${process.env.GITHUB_REPOSITORY}/.github/workflows/publish-companion.yml@${process.env.GITHUB_REF}`;
  const known_headers = new Set([
    'alg',
    'typ',
    'cty',
    'kid',
    'jku',
    'jwk',
    'x5u',
    'x5c',
    'x5t',
    'x5t#S256',
    'crit',
    'enc',
    'zip',
  ]);
  const checks: Record<string, boolean> = {
    header_rs256: header.alg === 'RS256',
    header_jwt: header.typ === 'JWT',
    header_fields_known: Object.keys(header).every((key) => known_headers.has(key)),
    header_key_sources_absent: !header.jku && !header.jwk && !header.x5u,
    header_critical_absent: !header.crit,
    header_encryption_absent: !header.enc && !header.zip,
    issuer_matches: claims.iss === 'https://token.actions.githubusercontent.com',
    audience_matches: claims.aud === audience,
    not_expired: claims.exp > now,
    positive_lifetime: claims.exp > claims.iat,
    lifetime_at_most_600: claims.exp - claims.iat <= 600,
    iat_not_future: claims.iat <= now + 30,
    nbf_not_future: claims.nbf <= now + 30,
    nbf_before_expiry: claims.nbf < claims.exp,
    runner_github_hosted: claims.runner_environment === 'github-hosted',
    job_workflow_ref_absent: claims.job_workflow_ref == null,
    job_workflow_ref_matches: claims.job_workflow_ref === claims.workflow_ref,
    repository_id_matches: String(claims.repository_id) === process.env.GITHUB_REPOSITORY_ID,
    owner_id_matches: String(claims.repository_owner_id) === process.env.GITHUB_REPOSITORY_OWNER_ID,
    repository_matches: claims.repository === process.env.GITHUB_REPOSITORY,
    workflow_matches: claims.workflow_ref === expected_workflow,
    ref_matches: claims.ref === process.env.GITHUB_REF,
    event_matches: claims.event_name === process.env.GITHUB_EVENT_NAME,
  };
  for (const field of [
    'iss',
    'aud',
    'sub',
    'jti',
    'repository_id',
    'repository_owner_id',
    'repository',
    'workflow_ref',
    'ref',
    'event_name',
    'runner_environment',
  ]) {
    checks[`${field}_is_string`] = typeof claims[field] === 'string';
  }
  for (const field of ['exp', 'iat', 'nbf'])
    checks[`${field}_is_integer`] = Number.isSafeInteger(claims[field]);
  console.log(JSON.stringify(checks));
}

diagnose().catch(() => {
  console.error('Identity diagnostics failed; no token data logged.');
  process.exitCode = 1;
});

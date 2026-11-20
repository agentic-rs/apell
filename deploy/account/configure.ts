// Configuration for the Logto v1.46.0 Management API used by this deployment.

const smtp_factory = 'simple-mail-transfer-protocol';
const github_factory = 'github-universal';
const github_id = 'github';

type Connector = {
  id: string;
  connectorId: string;
  target: string;
  type: 'Email' | 'Sms' | 'Social';
  isDemo?: boolean;
  enableTokenStorage: boolean;
  config: Record<string, unknown>;
};

type SignInExperience = {
  signIn: { methods: { identifier: string; password: boolean; verificationCode: boolean }[] };
  signUp: { identifiers: string[]; password: boolean; verify: boolean };
  signInMode: string;
  socialSignIn: { automaticAccountLinking: boolean; skipRequiredIdentifiers: boolean };
  socialSignInConnectorTargets: string[];
  passkeySignIn: { enabled: boolean; showPasskeyButton: boolean; allowAutofill: boolean };
};

type AccountCenter = {
  enabled: boolean;
  fields: Record<string, 'Off' | 'ReadOnly' | 'Edit'>;
};

class ConfigurationError extends Error {}

function required(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new ConfigurationError(`Set ${name} in the operator environment.`);
  return value;
}

function managementOrigin(): string {
  const url = URL.parse(required('ACCOUNT_MANAGEMENT_ENDPOINT'));
  const loopback = url && ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname);
  if (
    !url ||
    (url.protocol !== 'https:' && !(url.protocol === 'http:' && loopback)) ||
    url.username ||
    url.password ||
    url.pathname !== '/' ||
    url.search ||
    url.hash
  ) {
    throw new ConfigurationError(
      'Set ACCOUNT_MANAGEMENT_ENDPOINT to a loopback HTTP or HTTPS origin.',
    );
  }
  return url.origin;
}

function connectorConfiguration() {
  const from_email = required('ACCOUNT_EMAIL_FROM');
  if (!/^[^@\s<>,;]+@[^@\s<>,;]+$/.test(from_email)) {
    throw new ConfigurationError('Set ACCOUNT_EMAIL_FROM to a single sender email address.');
  }

  return {
    smtp: {
      host: 'smtp.resend.com',
      port: 465,
      secure: true,
      auth: { user: 'resend', pass: required('ACCOUNT_RESEND_API_KEY') },
      fromEmail: from_email,
      templates: ['Register', 'SignIn', 'ForgotPassword', 'Generic'].map((usage_type) => ({
        usageType: usage_type,
        contentType: 'text/plain',
        subject: 'Apell Account verification code',
        content: 'Your Apell Account verification code is {{code}}.',
      })),
    },
    github: {
      clientId: required('ACCOUNT_GITHUB_CLIENT_ID'),
      clientSecret: required('ACCOUNT_GITHUB_CLIENT_SECRET'),
    },
  };
}

async function run() {
  const args = process.argv.slice(2);
  const verify_only = args.length === 1 && args[0] === '--verify';
  if (args.length && !verify_only) {
    throw new ConfigurationError('Usage: bun deploy/account/configure.ts [--verify]');
  }

  const origin = managementOrigin();
  const token = required('ACCOUNT_MANAGEMENT_TOKEN');
  const configuration = verify_only ? undefined : connectorConfiguration();

  async function request<T = void>(method: 'GET' | 'POST' | 'PATCH', path: string, body?: object) {
    let response: Response;
    try {
      response = await fetch(`${origin}/api/${path}`, {
        method,
        redirect: 'error',
        headers: {
          authorization: `Bearer ${token}`,
          accept: 'application/json',
          ...(body && { 'content-type': 'application/json' }),
        },
        body: body && JSON.stringify(body),
        signal: AbortSignal.timeout(15_000),
      });
    } catch {
      throw new ConfigurationError(`${method} /api/${path} could not reach the Management API.`);
    }

    if (!response.ok || method !== 'GET') {
      // Connector API responses can include provider secrets.
      await response.body?.cancel().catch(() => undefined);
    }
    if (!response.ok) {
      throw new ConfigurationError(`${method} /api/${path} failed with HTTP ${response.status}.`);
    }
    return method === 'GET' ? ((await response.json()) as T) : (undefined as T);
  }

  function readConfiguration() {
    return Promise.all([
      request<Connector[]>('GET', 'connectors'),
      request<SignInExperience>('GET', 'sign-in-exp'),
      request<AccountCenter>('GET', 'account-center'),
    ]);
  }

  let [connectors, experience, account_center] = await readConfiguration();

  if (configuration) {
    const smtp = connectors.find(({ type, isDemo }) => type === 'Email' && !isDemo);
    const github = connectors.find(({ id }) => id === github_id);
    if (smtp && smtp.connectorId !== smtp_factory) {
      throw new ConfigurationError('Logto already has a different email connector.');
    }
    if (github && (github.connectorId !== github_factory || github.enableTokenStorage)) {
      throw new ConfigurationError(
        'The reserved GitHub connector ID has a different configuration.',
      );
    }
    if (
      connectors.some(({ id, connectorId }) => id !== github_id && connectorId === github_factory)
    ) {
      throw new ConfigurationError('The existing GitHub connector has a different callback ID.');
    }

    if (smtp) {
      await request('PATCH', `connectors/${encodeURIComponent(smtp.id)}`, {
        config: configuration.smtp,
      });
    } else {
      await request('POST', 'connectors', {
        connectorId: smtp_factory,
        config: configuration.smtp,
      });
    }

    if (github) {
      await request('PATCH', `connectors/${github_id}`, { config: configuration.github });
    } else {
      await request('POST', 'connectors', {
        id: github_id,
        connectorId: github_factory,
        config: configuration.github,
        enableTokenStorage: false,
      });
    }

    const social_targets = experience.socialSignInConnectorTargets.filter((target) =>
      connectors.some((connector) => connector.target === target && !connector.isDemo),
    );
    await request('PATCH', 'sign-in-exp', {
      signIn: {
        methods: [
          {
            identifier: 'email',
            password: false,
            verificationCode: true,
            isPasswordPrimary: false,
          },
        ],
      },
      signUp: { identifiers: ['email'], password: false, verify: true },
      signInMode: 'SignInAndRegister',
      socialSignIn: { automaticAccountLinking: false, skipRequiredIdentifiers: false },
      socialSignInConnectorTargets: [...new Set([...social_targets, github_id])],
      passkeySignIn: { enabled: true, showPasskeyButton: true, allowAutofill: true },
    });
    await request('PATCH', 'account-center', {
      enabled: true,
      fields: { ...account_center.fields, email: 'Edit', social: 'Edit', passkey: 'Edit' },
    });
    [connectors, experience, account_center] = await readConfiguration();
  }

  const verified_smtp = connectors.some(
    ({ connectorId, config }) =>
      connectorId === smtp_factory &&
      config.host === 'smtp.resend.com' &&
      config.port === 465 &&
      config.secure === true,
  );
  const verified_github = connectors.some(
    ({ id, connectorId, target, enableTokenStorage }) =>
      id === github_id &&
      connectorId === github_factory &&
      target === github_id &&
      !enableTokenStorage,
  );
  if (
    !verified_smtp ||
    !verified_github ||
    !experience.socialSignInConnectorTargets.includes(github_id)
  ) {
    throw new ConfigurationError(
      'Resend SMTP and GitHub connectors are not configured as expected.',
    );
  }
  const [email] = experience.signIn.methods;
  if (
    experience.signIn.methods.length !== 1 ||
    email?.identifier !== 'email' ||
    email.password ||
    !email.verificationCode ||
    experience.signUp.identifiers.length !== 1 ||
    experience.signUp.identifiers[0] !== 'email' ||
    experience.signUp.password ||
    !experience.signUp.verify ||
    experience.signInMode !== 'SignInAndRegister' ||
    experience.socialSignIn.automaticAccountLinking ||
    experience.socialSignIn.skipRequiredIdentifiers ||
    !experience.passkeySignIn.enabled ||
    !experience.passkeySignIn.showPasskeyButton ||
    !experience.passkeySignIn.allowAutofill
  ) {
    throw new ConfigurationError(
      'Passwordless sign-in or account linking is not configured as expected.',
    );
  }
  if (
    !account_center.enabled ||
    ['email', 'social', 'passkey'].some((field) => account_center.fields[field] !== 'Edit')
  ) {
    throw new ConfigurationError('Account Center is not configured as expected.');
  }

  console.log('Verified Logto settings for email codes, GitHub, passkeys, and Account Center.');
}

run().catch((error: unknown) => {
  console.error(
    error instanceof ConfigurationError ? error.message : 'Unexpected Logto configuration error.',
  );
  process.exitCode = 1;
});

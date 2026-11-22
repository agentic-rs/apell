# Apell Account

Apell Account is the shared OpenID Connect sign-in service for Apell and other
registered applications. The Logto deployment supports email verification codes,
passkeys, and GitHub. Account sign-in is optional for local applets and public
Store browsing and downloads.

Each application has a separate OIDC registration and exact callback. Native
clients should open the system browser and use authorization code with S256
PKCE. The Store owns its session and package permissions. GitHub Actions
continues to use the [CI publishing flow](store.md#configure-the-workflow).

## Stage the issuer

Run the [Compose deployment](../deploy/account/compose.yaml) with persistent
PostgreSQL storage. On an existing Account host, stage it with a separate Compose
project and a fresh database. Retain the previous issuer and its database for
rollback until the new sign-in flows have passed verification.

1. Create a private environment file from
   [the example](../deploy/account/.env.example). Set `ACCOUNT_ENDPOINT` to the
   canonical origin with no trailing slash. Use an HTTPS origin for public
   deployments and generate a separate `ACCOUNT_DB_PASSWORD` with
   `openssl rand -hex 32`.
2. Start the stack. Initialization seeds the fresh Logto database before
   the issuer starts.
3. Open the Admin Console through SSH port forwarding. The public issuer
   listens on host loopback port 5656 by default, and the Admin Console listens
   separately on loopback port 3002. Create the initial administrator through
   the private console before publishing the issuer.
4. Route the HTTPS hostname to the core port with
   [the Nginx example](../deploy/account/nginx.conf.example). Use the console's
   actual configured port for the tunnel if you change `ACCOUNT_ADMIN_PORT`.

```sh
cd deploy/account
docker compose --env-file .env.logto config --quiet
docker compose --env-file .env.logto up -d
curl -fsS http://127.0.0.1:5656/oidc/.well-known/openid-configuration
```

From the operator's machine, substitute the saved SSH destination in this
example and open [the local Admin Console](http://127.0.0.1:3002/).

```sh
ssh -N -L 3002:127.0.0.1:3002 account-host
```

Set up HTTPS and validate that public discovery reports the expected origin
with the `/oidc` issuer path. Persist and back up the `logto_identity` volume. It
contains account records, linked identities, connector settings, and signing keys.
[Logto deployment guide](https://docs.logto.io/logto-oss/deployment-and-configuration)

## Configure sign-in

- **Email code.** Add the SMTP connector and use `smtp.resend.com`, port 465
  with TLS, username `resend`, a Resend sending key as the password, and a
  From address on a Resend-verified domain. Enable email verification codes for
  sign-up and sign-in. Logto creates and verifies the codes, and Resend
  delivers them. [Logto SMTP connector](https://docs.logto.io/integrations/smtp) ·
  [Resend SMTP](https://resend.com/docs/send-with-smtp)
- **Passkey.** Enable passkey sign-in in **Sign-in & account**. Enable the
  **Continue with passkey** button so an enrolled passkey can authenticate on
  its own. [Logto passkey sign-in](https://docs.logto.io/end-user-flows/sign-up-and-sign-in/passkey-sign-in)
- **GitHub.** Add the GitHub OAuth App connector and add it to the sign-in page.
  Register the exact callback reported by Logto. A connector with ID
  `github` uses `/callback/github` on the Account origin. Disable automatic
  linking by matching email so users explicitly link their GitHub identity.
  [Logto GitHub connector](https://docs.logto.io/integrations/github) ·
  [Social account linking](https://docs.logto.io/end-user-flows/sign-up-and-sign-in/social-sign-in)
- **Account Center.** Enable Account Center and allow users to manage email,
  connections, and passkeys. The public configuration sends the root URL to
  `/account/security`. [Account Center settings](https://docs.logto.io/end-user-flows/account-settings/by-account-center-ui)

Operators can apply the same settings with
[`configure.ts`](../deploy/account/configure.ts). In a private environment file,
set `ACCOUNT_MANAGEMENT_ENDPOINT` to the Logto core origin accessible to the
operator and `ACCOUNT_MANAGEMENT_TOKEN` to an authorized Management API token.
Supply `ACCOUNT_RESEND_API_KEY`, `ACCOUNT_EMAIL_FROM`,
`ACCOUNT_GITHUB_CLIENT_ID`, and `ACCOUNT_GITHUB_CLIENT_SECRET` for configuration.
Use `--verify` with only the endpoint and token to check existing settings.
[Management API authentication](https://docs.logto.io/integrate-logto/interact-with-management-api)

```sh
bun --env-file=deploy/account/.env.providers deploy/account/configure.ts
```

Verify email-code sign-in with a test mailbox, enroll and use a passkey in a
fresh session, and complete GitHub sign-in before connecting production clients.
Use the final hostname to enroll production passkeys and verify the registered
GitHub callback. The browser should reach Account Center and allow linking
GitHub from an authenticated Account session.

## Register the Store and other clients

Install the CLI from this checkout to register OIDC applications through the
Account Management API.

```sh
cargo install --locked --path crates/apell-cli
```

Registration is an operator operation. Export `ACCOUNT_MANAGEMENT_ENDPOINT`
and `ACCOUNT_MANAGEMENT_TOKEN` in the operator's shell, using an authorized
Logto Management API token. These are the same environment variables used by
`configure.ts`. The endpoint is the Logto core origin, using HTTPS or loopback
HTTP through an SSH tunnel. With a tunnel to the core port, the endpoint can be
`http://localhost:5656`. [Management API authentication](https://docs.logto.io/integrate-logto/interact-with-management-api)

Register a confidential web service with its exact sign-in and sign-out
callbacks.

```sh
apell auth app register \
  --name "Example service" \
  --type web \
  --redirect-uri https://service.example.invalid/auth/callback \
  --post-logout-redirect-uri https://service.example.invalid/ \
  --output .env.example-service-oidc.json
```

Use `--type native` for a mobile or desktop application and `--type spa` for a
browser application. Native clients can register custom URI schemes and
loopback development callbacks.

```sh
apell auth app register \
  --name "Example native app" \
  --type native \
  --redirect-uri org.example.app:/auth/callback \
  --output .env.example-native-oidc.json
```

Repeat `--redirect-uri` or `--post-logout-redirect-uri` to register multiple
exact callbacks. Registrations use third-party consent for the standard
`openid`, `profile`, and `email` scopes by default. Add `--first-party` for a
trusted service operated with Account. `apell package oidc` configures a
package's GitHub Actions publisher separately from these Account clients.

The command discovers the public issuer and writes a new JSON file containing
`issuer`, `client_id`, `client_type`, `redirect_uris`, and
`post_logout_redirect_uris`. Web clients also receive `client_secret`. Public
native and SPA clients use authorization code with S256 PKCE. The output
file has mode `0600`, and its parent directory must exist. Existing output
files are preserved. The `.env.*` output names above are ignored by Git.
Terminal output contains the registration summary and client ID, with
`--json` available for scripts.

Use discovery at `/oidc/.well-known/openid-configuration`. Register one
confidential web client for the Store with its own secret and exact callback
`/api/v1/auth/account/callback` under the Store origin. Use the issuer's full
URL, including the `/oidc` path, as the `issuer` in the Store's
`APELL_STORE_ACCOUNT_OIDC_FILE`. The Store uses per-request state and nonce,
and verifies the ID token before creating its own session.

The stable Account identity is the signed `(iss, sub)`. The Store uses it to
resolve existing Store user bindings. For a first sign-in without a binding,
the Store reads the linked numeric GitHub ID from Account UserInfo after
matching its subject to the ID token. Bound Accounts can use email codes,
passkeys, or GitHub sign-in with the same package access.
[Store sign-in and operator setup](store-accounts.md)

Register native Apell and other services separately as they adopt Account.
Each service keeps its own authorization rules and scopes.

## Verify locally

Run `bun deploy/account/smoke.ts` from the repository root with Bun and Docker
Compose installed. Use the isolated smoke test before changing production
configuration, then verify the three real sign-in methods in a browser before
cutover.

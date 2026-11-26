# Apell Account

Apell Account is the shared OpenID Connect sign-in service for Apell and other
registered applications. The Logto deployment supports email verification codes,
passkeys, and GitHub. Account sign-in is optional for local applets and public
Store browsing and downloads.

Each application has a separate OIDC registration. Browser redirect flows
register an exact callback, and public redirect clients use S256 PKCE. The Apell
shell uses device codes. The Store owns its session and package permissions. GitHub Actions
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
`issuer`, `client_id`, `client_type`, `device_flow`, `redirect_uris`, and
`post_logout_redirect_uris`. Web clients also receive `client_secret`. Native
clients registered with callbacks and SPA clients use S256 PKCE. The output
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

## Sign in to Apell and sync model settings

Register a dedicated first-party Native client with device flow for the shell.
Logto sets the authorization flow at creation. The equivalent Console option is
**Device flow** when creating a Native application. [Logto device flow](https://docs.logto.io/quick-starts/device-flow)

```sh
apell auth app register \
  --name "Apell Shell Device" \
  --type native --device-flow --first-party \
  --output deploy/account/.env.shell-device-oidc.json
```

Set up a Logto API resource for config sync with `config:read` and
`config:write`. Assign those permissions through a role to the Accounts
approved for sync. Keep that role out of the default assignments during a
pilot. [Logto API resource setup](https://docs.logto.io/authorization/global-api-resources)

For local builds, put these public values in the ignored
`deploy/account/.env.shell-pilot` file as `NAME=value` entries. This is the shared
profile for simulator, iPhone, Android, and macOS builds. `apell dev` and
`pnpm tauri` pick it up from the shell checkout. Exported build variables
override the file. The shell polls Logto while the user approves the device code
in a browser and then keeps session credentials in the device's secure store.

| Variable                      | Value                                                        |
| ----------------------------- | ------------------------------------------------------------ |
| `APELL_ACCOUNT_ISSUER`        | Canonical HTTPS Account issuer including `/oidc`             |
| `APELL_ACCOUNT_CLIENT_ID`     | Public Native Device flow client ID                          |
| `APELL_SYNC_URL`              | HTTPS origin of the Store serving config sync                |
| `APELL_SYNC_RESOURCE`         | Exact Logto API resource identifier for sync                 |
| `APELL_KEY_EXCHANGE_RESOURCE` | Exact Account API resource identifier for encrypted key sync |

Configure `APELL_STORE_CONFIG_SYNC_FILE` on the Store host with the same `issuer`,
`client_id`, and resource `audience` as the shell. The Store validates the client
ID, audience, and sync scopes and uses the token's issuer and subject for the
settings namespace.

In **Settings → Apell Account**, choose **Sign in** to see the browser address
and code. Enter the code on the sign-in page on this or another device. When
Apell shows the signed-in account, turn on **Sync model settings**. Apell
publishes local changes and checks for cloud changes while it runs. Use **Sync
now** to check immediately. Apell shows changes on this device that are still
waiting to sync.
Model settings sync includes connection labels, built-in provider and model
selections, and the preferred active model. API keys can be synced separately
through **Encrypted API keys**. A usable local model can serve as a fallback
until the preferred model’s key arrives. Apell activates the restored model
while the settings still match the last sync.
When devices make conflicting edits, or the cloud copy removes a connection
from this device, Apell keeps the local settings and shows **Review changes**.
This remains available after restarting Apell. Open it to compare the changed
connections and preferred model, then choose which copy to use.
Applying the cloud copy removes any local-only connections listed in the review
and their saved credentials. **Refresh review** fetches a new comparison so you
can review it again before choosing. The Store records each version with its
parent revision and applies updates only when the expected cloud revision still
matches.

## Sync encrypted API keys

In **Settings → Apell Account → Encrypted API keys**, signed-in devices show
**Verified** or **Unverified**. On the first device, choose **Verify this
device** to create the vault. On an additional device, choose **Pair this
device** and open **Approve a device** on a verified device. Compare the six
digits on both screens and approve the matching request. When all verified
devices are unavailable, choose **Recover access** and verify your Account
again in the browser. Return to Apell after the browser confirms verification.
If your pairing code expires, choose **Get a new code**. If browser verification
expires or is incomplete, restart it from Apell.

Apell holds the vault key in native secure storage. It uses HKDF-SHA256 to
derive separate keys for encrypting provider API keys, proving device approval,
and tracking credential changes. New API-key backups use the derived encryption
key. Apell can also restore earlier encrypted backups.

The Account relay keeps a copy of the vault key encrypted under a
separate server key. Account can release that copy to the requesting device
after fresh verification. The Store holds encrypted provider keys and revision
metadata. An existing device with a legacy vault can enroll that same vault
through Account verification and keep its existing encrypted backups.

Eligible static API keys sync automatically on verified devices. Enable **Sync
model settings** to bring missing built-in provider connections onto a new
device. If Apell is waiting for a provider connection, check **Model settings**
on the device where you added it and resolve any pending review there. Use
**Retry key sync** on the waiting device to check again immediately. The
**Encrypted API keys** section shows checking, waiting, and sync errors
separately from device verification. An unverified device shows **Check again**
if Apell cannot reach the service to determine how to verify it.
Apell keeps local and cloud copies when both have changed
independently. Provider OAuth sessions and custom endpoint credentials stay
on the device.

Closing a pairing or recovery sheet ends that verification request on this
device. Start a new request to continue. Signing out interrupts outstanding
relay waits, including the approval list and verification checks. Apell lets a
local vault or credential write already in progress finish before ending the
session.

If Apell cannot reliably record key changes on a device, it pauses encrypted
API key sync on that device so you can continue managing its local model
connections. The pause persists across restarts and Account sign-ins. A
verified device can still approve another device while its own sync is paused.

### Enable encrypted key sync in a deployment

1. Add `credentials:read` and `credentials:write` to the existing Store config
   sync API resource. Register a separate Account API resource on the Account
   HTTPS origin for `key_exchange:read` and `key_exchange:write`. Assign these
   permissions to the approved Accounts.
2. Build both binaries with `cargo build --release --locked -p apell-store --bins`. The updated Store
   migrates its database on startup. Run the key relay on the Account host
   with its own data directory and OIDC settings. See the [service example](../deploy/account/apell-account-keys.service),
   [OIDC settings](../deploy/account/keys-oidc.example.json), and
   [Account proxy configuration](../deploy/account/key-exchange.nginx.conf.example).
3. Create a random 32-byte server wrapping key as one unpadded base64url line.
   Supply its protected file through `APELL_ACCOUNT_KEY_EXCHANGE_MASTER_KEY_FILE`.
   Back up the key separately from the relay database.
4. Register a first-party confidential web client for the recovery callback at
   `/api/v1/key-vault/managed/recovery/callback` on the Account origin. Supply
   its `client_id` and `client_secret` JSON through
   `APELL_ACCOUNT_KEY_EXCHANGE_RECOVERY_OIDC_FILE`. Use systemd
   `LoadCredential` to give the relay its two files while keeping them
   protected on disk.
5. Set `APELL_KEY_EXCHANGE_RESOURCE` to the exact Account key relay audience in
   the native build, for example `https://account.example.invalid/api/v1/key-vault`.
   Sign in again on that build so Logto grants the configured resources and
   scopes. Builds without this setting continue to support model settings sync.

### Check the build profile

Run `pnpm check:shell-profile` to validate all five settings. Set
`APELL_REQUIRE_KEY_SYNC=1` to run the same check before a local native build.
The check fails before packaging if a required setting is missing or invalid.
Apell Account sign-in remains optional after installation.

```sh
APELL_REQUIRE_KEY_SYNC=1 apell dev --target 'Apell iPhone 16 Pro'
APELL_REQUIRE_KEY_SYNC=1 apell dev --target device
APELL_REQUIRE_KEY_SYNC=1 pnpm tauri android build --ci --debug --target aarch64 --apk
APELL_REQUIRE_KEY_SYNC=1 pnpm desktop:build --debug --bundles app
```

For GitHub Actions, save the complete profile as one repository Actions secret,
`APELL_SHELL_BUILD_PROFILE`. It contains the public native client ID and service
addresses. When supplied to a native build, it replaces the local profile and
individual build variables. CI checks the saved profile on the main branch
and uses test values for pull requests.

```sh
gh secret set APELL_SHELL_BUILD_PROFILE < deploy/account/.env.shell-pilot
```

## Verify locally

For day-to-day changes, the [local Docker workflow](local-development.md) runs
the Store and key relay with a synthetic Account issuer. It uses local data and
credentials. The following smoke test checks the separate Logto deployment.

Run `bun deploy/account/smoke.ts` from the repository root with Bun and Docker
Compose installed. Use the isolated smoke test before changing production
configuration, then verify the three real sign-in methods in a browser before
cutover.

To verify that a registered device flow client can request a code and poll for
approval, pass the Account origin and its client ID to the smoke test.

```sh
bun deploy/account/smoke.ts --endpoint https://auth.example.invalid --device-client-id "$APELL_ACCOUNT_CLIENT_ID"
```

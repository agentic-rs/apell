# Apell Account

Apell Account is a standalone [OpenID Connect issuer](https://dexidp.io/docs/openid-connect/)
powered by Dex. It signs people in through GitHub and exchanges authorization
codes for signed identity tokens. An account is optional for local applets and
public Store browsing and downloads.

The deployment registers `apell-store-web` for developer sign-in. The Store's
website and CLI currently use the [direct GitHub login](store-accounts.md). A
separate cutover will have the website use this issuer and create its usual Store
session, including for browser approval of CLI access. The Store owns publisher
permissions. CI authenticates through the [GitHub Actions publishing flow](store.md#configure-the-workflow).

## Run the issuer

Use Docker Compose on the Account host with an HTTPS reverse proxy. The Compose
deployment runs Dex and a dedicated PostgreSQL database. PostgreSQL is reachable
only on the private Compose network. The issuer's HTTP listener binds to loopback
for the proxy.

1. Choose the canonical HTTPS origin with no trailing slash and set it as
   `ACCOUNT_ISSUER`.
2. Create a new [GitHub OAuth App](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/creating-an-oauth-app).
   Use the Account origin as its homepage and append `/callback` for its exact
   **Authorization callback URL**. Keep the Store's current OAuth App in place
   during the cutover.
3. In `deploy/account`, run `install -m 600 .env.example .env`. Put the new
   GitHub application's client ID and secret in `ACCOUNT_GITHUB_CLIENT_ID`
   and `ACCOUNT_GITHUB_CLIENT_SECRET`. Generate separate values for
   `ACCOUNT_DB_PASSWORD` and `ACCOUNT_STORE_CLIENT_SECRET` with `openssl rand -hex 32`.
   For `ACCOUNT_SESSION_KEY`, use `openssl rand -hex 16`, which produces the 32
   ASCII bytes expected by Dex. Keep these values and the database backups private.
   The Store server will need its client secret at cutover.
4. Set `ACCOUNT_STORE_REDIRECT_URI` to the Store's canonical HTTPS origin plus
   `/api/v1/auth/account/callback`. This is the future Store callback. The GitHub
   application's callback points to Account.
5. Start the services and configure the HTTPS proxy using
   [the Nginx location example](../deploy/account/nginx.conf.example). Keep the
   proxy upstream port in sync if you change `ACCOUNT_PORT`.

```sh
cd deploy/account
docker compose up -d
curl -fsS https://auth.example.invalid/.well-known/openid-configuration
```

Substitute your Account origin in the discovery URL. Verify that its `issuer`
matches `ACCOUNT_ISSUER` and that `jwks_uri` resolves through HTTPS. The proxy
example suppresses request logging so OAuth codes and state do not enter logs.
Persist and back up the `identity` volume to retain signing keys and login
sessions across upgrades. Store package data and applet data belong to their
respective services.

## Store client contract

Use OpenID Connect discovery for the authorization, token, and signing key URLs.
Use the authorization code flow with S256 PKCE and per-request state and nonce.
Redeem the code from the Store server with its confidential client credentials.
Request the `openid profile federated:id` scopes. Validate the returned ID token's
signature, issuer, audience, expiry, and nonce before opening a Store session.
When present, validate `at_hash` against the access token from the same exchange.

Dex's GitHub connector requests read-only access to the user's GitHub email. The
Store's requested scopes leave email out of its ID token. Keep `offline_access`
out of the Store request because the connector can retain upstream credentials
when that scope is requested.

Use `(iss, sub)` as the Account identity. When linking a publisher to an
existing Store account, use the signed `federated_claims.user_id` only when
`federated_claims.connector_id` is `github` and the ID is a positive numeric GitHub
user ID. `preferred_username` supplies the current GitHub handle for display.
[Dex GitHub connector](https://dexidp.io/docs/connectors/github/) ·
[Dex scopes and claims](https://dexidp.io/docs/configuration/custom-scopes-claims-clients/)

Native sign-in, scoped credentials for service APIs, and sync can use separate
client registrations and authorization policies when their consumers are ready.

## Verify locally

Run `bun deploy/account/smoke.ts` from the repository root with Bun, Docker Compose,
and OpenSSL installed. The smoke test launches a disposable issuer and database,
uses a local GitHub fixture, and verifies code exchange and the signature and
identity claims on the resulting ID token.

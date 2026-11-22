# Store accounts and publishing access

The website signs publishers in through Apell Account. After the Account is
bound to a Store user, email codes, passkeys, and GitHub sign-in all open the
same Store account. On the first sign-in for an unbound Account, link GitHub
in Account Security so the Store can find the matching user. Public browsing,
downloads, and the native Apell Store remain available without an account.

The [CLI account commands](store-cli.md) support browser approval, registration,
publishing, OIDC settings, and maintainer management using the same permissions.

## Register and publish

1. Sign in and open **My packages → Register package**.
2. Choose the permanent applet ID used in your package's `manifest.json`.
3. Publish through the package's **Publish release** form, or configure **GitHub
   Actions** and publish from CI.

Any signed-in Store user can register an available ID. Registration immediately
reserves that ID and assigns its creator as owner. A public catalog entry appears
after the first successful release. IDs already registered, published, or reserved
during migration cannot be claimed by another signup. The small Store limit of
256 packages includes unpublished registrations.

Published versions remain immutable. Moving to another repository does not change
the package ID or release history; continue with a new version for changed content.

## Maintainers and ownership

Each package has one owner and up to 31 additional maintainers. The owner and all
maintainers can publish releases, connect/change/disconnect a GitHub Actions
publisher, invite maintainers, cancel invitations, and remove other maintainers.
Maintainers can leave by removing themselves. The owner cannot be removed.

Enter a GitHub username to invite someone. The server resolves its numeric user ID;
only that account can accept from **My packages → Invitations**. Invitations expire
after seven days and do not grant access until accepted. No email is sent.

The current owner may transfer ownership to an existing maintainer. The recipient
must accept the ownership invitation. The previous owner becomes a maintainer,
and the package always retains one owner. Removing a member also cancels pending
invitations involving that member. Accepting an invitation rechecks the inviter's
current authority.

Package mutations are recorded in a local database audit table. Membership is
checked again within the final publication transaction, so a removed maintainer
cannot finish an upload that was started before removal.

## GitHub publishing connections

In package settings, enter a public `OWNER/REPO`, workflow filename, allowed refs
and events, and optional environment. The Store looks up the numeric repository
and owner IDs and saves the connection. It requests no repository permissions at
sign-in; this first account UI supports public repository lookup only.

Changes apply immediately. Changing or disconnecting the connection invalidates
previously issued CI publishing credentials. The final upload transaction also
checks the connection revision, closing the gap between authentication and saving
a large upload. A repository rename under the same owner remains authorized;
a different repository or an ownership transfer requires saving updated settings.

Removing a Store maintainer does not revoke a trusted GitHub workflow: package
membership and repository publishing are separate grants. Review the publishing
connection when changing maintainers. Disconnecting GitHub keeps existing releases
available, and members can still upload through the website. The private operator
token remains an independent recovery/publishing capability.

See [the CI publishing guide](store.md#configure-the-workflow) for workflow setup.

## Operator setup

Register a first-party **Traditional Web** client in Apell Account. Use the Store
origin plus `/api/v1/auth/account/callback` as its exact redirect URI. Save the
issuer URL, client ID, and client secret in a private JSON file readable by the
Store service account. [Account client setup](account.md#register-the-store-and-other-clients)

```json
{
  "issuer": "https://auth.example.invalid/oidc",
  "client_id": "replace-with-the-oauth-client-id",
  "client_secret": "replace-with-the-oauth-client-secret"
}
```

Set `APELL_STORE_PUBLIC_URL` and `APELL_STORE_ACCOUNT_OIDC_FILE` in the service
environment. The Account file takes precedence over
`APELL_STORE_GITHUB_OAUTH_FILE`, which remains an operator rollback option for
direct GitHub login. Keep actual credentials and deployment values on the server.
Restart after changing the provider.

The server uses browser-bound, single-use state, a nonce, and S256 PKCE. It
validates the signed ID token and looks up the Account issuer and subject in
the Store's bindings. For an unbound Account, it checks that Account UserInfo
describes the same subject before reading the linked numeric GitHub ID. The
Store keeps the original user when the Account's social connections change.
Store sessions use opaque random credentials, stored as hashes
in SQLite, and host-only Secure/HttpOnly/SameSite cookies. Sessions last seven days;
sign-out deletes the current session. Browser mutations require both the exact
Store origin and a session-bound `x-csrf-token` header. Credentials never belong in
browser local storage.

Use the updated [Nginx template](../deploy/store/nginx.conf.example): it suppresses
access/error logging on the OAuth callback so authorization codes and state do not
enter proxy logs. Any additional proxy or monitoring service must likewise avoid
recording callback query strings or authentication headers. The template also
limits authorization creation by the connecting client's address to six requests
per minute with a small burst allowance. Preserve this limit, or an equivalent
trusted-edge limit, when using another proxy. Keep the application listener private
so anonymous clients cannot bypass it and exhaust the shared authorization pool.

### Migrate an existing Store

Back up the existing database before deploying. Schema changes add account and
package registry tables without replacing the catalog, release files, or icons.
All existing applet IDs become reserved. Keep the existing
`APELL_STORE_TRUSTED_PUBLISHERS_FILE` configured for the first startup: its policies
are imported once into SQLite. Later startup ignores policy edits in this file;
the website becomes authoritative. Remove this import setting after migration.

To assign reserved existing packages explicitly, prepare a private owner file:

```json
{
  "github_id": 123456789,
  "github_login": "example-owner"
}
```

The operator must verify the numeric identity. Set
`APELL_STORE_BOOTSTRAP_OWNER_FILE` for the migration startup, then remove the setting
and private file. It assigns only unowned reserved packages; it never changes a
package that already has an owner. The first website signup never claims legacy
packages automatically.

Back up `catalog.sqlite3` consistently using SQLite's backup mechanism or with the
service stopped; include release files and icons. Treat sessions, profile data,
membership, and the audit log as private operational data.

## Account API

All fields use snake_case. Account/authentication responses use `Cache-Control:
no-store`. Browser sessions use cookies; CLI sessions use separate account bearer
credentials. Administrative publishing tokens do not grant account management
access. Browser mutations require Origin and CSRF checks; explicitly supplied CLI
bearer credentials do not use browser cookies. CLI approval and credential
management in the browser always require a browser session and CSRF protection.

| Method       | Path under `/api/v1`                                 | Behavior                                                 |
| ------------ | ---------------------------------------------------- | -------------------------------------------------------- |
| GET          | `/auth/me`                                           | Login provider, availability, current user, CSRF token   |
| GET          | `/auth/account/login`                                | Start Apell Account sign-in                              |
| GET          | `/auth/account/callback`                             | Complete sign-in and return to the Store or CLI approval |
| GET          | `/auth/account/security`                             | Open Apell Account Security to link GitHub               |
| GET          | `/auth/github/login` and `/auth/github/callback`     | Direct GitHub sign-in in legacy provider mode            |
| POST         | `/auth/logout`                                       | Delete the current session                               |
| GET / POST   | `/account/packages`                                  | List memberships/invitations; register an ID             |
| GET          | `/account/packages/{id}`                             | Read package management details                          |
| POST         | `/account/packages/{id}/publish`                     | Multipart release upload, bound to this package          |
| PUT / DELETE | `/account/packages/{id}/publisher`                   | Change or disconnect GitHub publishing                   |
| POST         | `/account/packages/{id}/members`                     | Invite a GitHub user                                     |
| DELETE       | `/account/packages/{id}/members/{github_id}`         | Remove a maintainer                                      |
| DELETE       | `/account/packages/{id}/invitations/{invitation_id}` | Cancel a package invitation                              |
| POST         | `/account/packages/{id}/ownership`                   | Invite an existing maintainer to take ownership          |
| POST         | `/account/invitations/{id}/accept`                   | Accept an invitation addressed to the current user       |
| DELETE       | `/account/invitations/{id}`                          | Decline an invitation                                    |

Registration takes `{ "applet_id": "example" }`; member invitations take
`{ "github_login": "example-maintainer" }`; ownership transfers take
`{ "github_id": 123456789 }`. Publisher changes take `repository`, `workflow`,
`allowed_refs`, `event_names`, and nullable `environment`. Other account mutations
take `{}`. Browser mutations require `x-csrf-token` and the browser's same-origin
Origin header; multipart uploads retain the existing `package`, `metadata`, and optional
`icon` fields. OAuth client secrets and Store session cookies are never API results.

### CLI authorization

The CLI starts a ten-minute authorization request, opens the Store's approval
page, and polls with a separate high-entropy secret. The browser shows only the
human verification code. Approval requires a Store browser session. Account
sign-in preserves the code through the redirect, including when the user must
link GitHub and retry sign-in.

| Method | Path under `/api/v1`                        | Behavior                                                     |
| ------ | ------------------------------------------- | ------------------------------------------------------------ |
| POST   | `/auth/cli/start`                           | Create a temporary authorization request                     |
| POST   | `/auth/cli/poll`                            | Poll with `device_code`; issue one credential after approval |
| GET    | `/auth/cli/me`                              | Read the CLI identity and expiry using its bearer credential |
| POST   | `/auth/cli/logout`                          | Revoke the current CLI credential                            |
| GET    | `/account/cli/requests/{user_code}`         | Inspect a request using a browser session                    |
| POST   | `/account/cli/requests/{user_code}/approve` | Approve from the browser with CSRF protection                |
| POST   | `/account/cli/requests/{user_code}/deny`    | Deny from the browser with CSRF protection                   |
| GET    | `/account/cli/credentials`                  | List the browser user's CLI credentials                      |
| DELETE | `/account/cli/credentials/{id}`             | Revoke one of the browser user's CLI credentials             |

CLI credentials last seven days, are stored only as hashes on the server, and
cannot approve further CLI credentials or manage browser sessions. An explicit
invalid bearer credential never falls back to a browser cookie. Account and
publishing requests check current package permissions. The existing GitHub
Actions OIDC and private operator credentials keep their separate scopes.

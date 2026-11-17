# Store accounts and publishing access

The website uses GitHub sign-in. Public browsing, downloads, and the native Apell
Store remain available without an account. Sign-in identifies a person by their
numeric GitHub user ID; changing a GitHub username does not change ownership.

## Register and publish

1. Sign in and open **My packages → Register package**.
2. Choose the permanent applet ID used in your package's `manifest.json`.
3. Publish through the package's **Publish release** form, or configure **GitHub
   Actions** and publish from CI.

Any signed-in GitHub user can register an available ID. Registration immediately
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

Create a dedicated GitHub **OAuth App** using
[GitHub's registration instructions](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/creating-an-oauth-app):

- Homepage: the canonical HTTPS Store origin, such as `https://store.example.invalid`.
- Callback: `https://store.example.invalid/api/v1/auth/github/callback`.
- Exact callback matching; device flow disabled. Expiring access tokens may remain enabled.

Create a private server-local JSON file, readable by the service account:

```json
{
  "client_id": "replace-with-the-oauth-client-id",
  "client_secret": "replace-with-the-oauth-client-secret"
}
```

Set `APELL_STORE_PUBLIC_URL` and `APELL_STORE_GITHUB_OAUTH_FILE` in the service
environment. Never commit actual credentials, deployment values, or account
identities. Restart after changing OAuth configuration. Without OAuth credentials,
public browsing and configured CI publishing continue while sign-in is unavailable.

The server uses browser-bound, single-use OAuth state and S256 PKCE. GitHub's token
is used only to fetch the current identity, then discarded. No email or repository
scope is requested. Store sessions use opaque random credentials, stored as hashes
in SQLite, and host-only Secure/HttpOnly/SameSite cookies. Sessions last seven days;
sign-out deletes the current session. Browser mutations require both the exact
Store origin and a session-bound `x-csrf-token` header. Credentials never belong in
browser local storage.

Use the updated [Nginx template](../deploy/store/nginx.conf.example): it suppresses
access/error logging on the OAuth callback so authorization codes and state do not
enter proxy logs. Any additional proxy or monitoring service must likewise avoid
recording callback query strings or authentication headers.

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
no-store`. Browser sessions use cookies; administrative bearer tokens do not grant
access to account management endpoints.

| Method       | Path under `/api/v1`                                 | Behavior                                             |
| ------------ | ---------------------------------------------------- | ---------------------------------------------------- |
| GET          | `/auth/me`                                           | Login availability, current user, session CSRF token |
| GET          | `/auth/github/login`                                 | Start GitHub sign-in                                 |
| GET          | `/auth/github/callback`                              | Complete sign-in; fixed redirect to account page     |
| POST         | `/auth/logout`                                       | Delete the current session                           |
| GET / POST   | `/account/packages`                                  | List memberships/invitations; register an ID         |
| GET          | `/account/packages/{id}`                             | Read package management details                      |
| POST         | `/account/packages/{id}/publish`                     | Multipart release upload, bound to this package      |
| PUT / DELETE | `/account/packages/{id}/publisher`                   | Change or disconnect GitHub publishing               |
| POST         | `/account/packages/{id}/members`                     | Invite a GitHub user                                 |
| DELETE       | `/account/packages/{id}/members/{github_id}`         | Remove a maintainer                                  |
| DELETE       | `/account/packages/{id}/invitations/{invitation_id}` | Cancel a package invitation                          |
| POST         | `/account/packages/{id}/ownership`                   | Invite an existing maintainer to take ownership      |
| POST         | `/account/invitations/{id}/accept`                   | Accept an invitation addressed to the current user   |
| DELETE       | `/account/invitations/{id}`                          | Decline an invitation                                |

Registration takes `{ "applet_id": "example" }`; member invitations take
`{ "github_login": "example-maintainer" }`; ownership transfers take
`{ "github_id": 123456789 }`. Publisher changes take `repository`, `workflow`,
`allowed_refs`, `event_names`, and nullable `environment`. Other account mutations
take `{}`. All mutations require `x-csrf-token` and the browser's same-origin Origin
header; multipart uploads retain the existing `package`, `metadata`, and optional
`icon` fields. OAuth client secrets and Store session cookies are never API results.

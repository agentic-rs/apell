# Apell Store

The store serves a public applet catalog, version history, and `.apel` downloads.
Publishing uses a signed-in package maintainer, GitHub Actions trusted publishing, or an operator token. Applets remain in their own repositories;
publishing a release does not require changing the shell or landing-page repository.

The website supports search, Stable and Nightly releases, declared permissions,
release notes, and SHA-256 checksums. Applets can also be browsed, installed, and
updated from **Store** inside Apell. Website downloads remain installable through
**+ → Install an .apel file**. Existing file and GitHub installation still work.

## Browse and install inside Apell

Open **Store** from the library and save the store's HTTPS address. The address
stays in the device's local settings; browsing and installation never require a
publishing token. Search the catalog, open an applet, and review its version,
release notes, and declared access before choosing **Install**.

Apell saves the catalog, recently viewed applet details, and browsing preferences
on this device. Returning to the Store restores the view while Apell checks for
changes. Saved listings can still be browsed if the Store is unreachable.
Installation and updates require a fresh review of the selected release.
The Store website also saves public listings in the browser.

The catalog lists every published applet once, including Nightly-only applets.
Open an applet to choose its release channel and see that channel’s version
history. Apell recommends Stable for new installs when available. For installed
applets, **Updates** follows the channel recorded on this device.
Review an update's permissions, including newly declared access, then choose
**Update**. Updates are manual, and existing applet data is retained.
If the old version is running, choose **Restart applet** after installing to
open the new package and backend. This leaves Apell and other applets running.

Each Store installation records the Store address, applet ID, and channel.
To change channels inside a running applet, open **About this applet** from the
three-dot menu, choose a different **Installed channel**, and review and install
the offered release. About checks the Store that supplied this applet, even if
you now browse a different Store. Choose **Restart applet** to use the new
version. You can also switch channels on the applet’s Store page.
The applet keeps its data and remembered permissions. Apell prevents downgrades,
so you may need to wait for a newer Stable version to switch back.
Previously installed Preview applets
now follow Nightly. Changing the configured store does not change installed applets' update
sources. To move an applet from a file, a GitHub release, or another store, open its
page, choose a Store release, and confirm **Switch to this Store**. This keeps the
applet's saved data and changes its source for future updates. The selected release
can access that data, including secure storage if declared; remembered device
permissions from the old source do not carry over. A source switch cannot
downgrade an installed applet. Automatic background updates are not included.

If an install download is interrupted, **Continue download** retries the
reviewed release from the last saved byte when the Store supports byte ranges.
Apell keeps one partial package in its local cache, including across app
restarts. If a Store cannot resume the transfer, Apell downloads the package
again and checks the full checksum before installing. On iOS, a download you
start can keep going while the phone is locked for as long as iOS grants
background time. If iOS pauses Apell, reopen it to continue.

The native host rechecks the reviewed release before downloading, then verifies
the package size, checksum, applet identity, version, and declared capabilities.
The installation also checks that the installed package has not changed since
review. A source switch checks the previous source and whether the applet had
been removed, including when it will restore retained data. Failed or stale
operations leave the previous installation intact.

## Accounts and package ownership

Sign in with GitHub on the website, open **My packages**, and choose **Register package**. Any signed-in GitHub user can register an available applet ID. The creator owns the package; its public listing appears after the first release. Owners and maintainers can publish files, configure GitHub Actions, and invite or remove other maintainers. Invitations require acceptance. The owner is protected from removal, and ownership transfer requires the current owner and the recipient to agree. See [accounts and publishing access](store-accounts.md) for setup, migration, and the full lifecycle.

## Publish from an applet repository

Install the CLI from an Apell checkout:

```sh
cargo install --locked --path crates/apell-cli
```

Build your applet and pack it with `apell pack` as described in the
[applet guide](../README.md#package-and-distribute). Create a listing JSON file:

```json
{
  "description": "A short description of what your applet does.",
  "project_url": "https://github.com/example/applet",
  "release_notes": "What changed in this release."
}
```

Package maintainers can upload a release through the website or sign in from the
CLI using browser approval:

```sh
apell login --store https://store.example.invalid
apell publish artifacts/example.apel \
  --store https://store.example.invalid \
  --metadata listing.json \
  --icon icon.png \
  --auth account
```

See the [CLI guide](store-cli.md) for package registration, GitHub publishing
connections, and maintainer commands. For operator recovery, keep the
administrator token in a private file outside source control, set
`APELL_STORE_PUBLISH_TOKEN_FILE` to that file, and explicitly use `--auth token`.

Replace the example URL with your store. The optional icon is a still PNG, at most
256 KiB and 1,024 × 1,024 pixels. The CLI validates the package before sending it,
requires HTTPS except for local loopback development, and refuses redirects so the
publishing token cannot be forwarded to another destination. For GitHub Actions,
use trusted publishing below. Do not put tokens in arguments, listings, packages,
logs, or repositories.

The server reads the ID, display name, version, permissions, and API version from
the package itself. Listing metadata is separate from `manifest.json`. Descriptions
are limited to 2,000 UTF-8 bytes and release notes to 10,000 bytes. Project links
must use HTTPS without URL credentials.

## Trusted publishing from GitHub Actions

A package owner or maintainer registers a trusted workflow in package settings. The workflow
requests a GitHub OIDC identity token and `apell publish` exchanges it for a
short-lived publishing credential restricted to that applet and package checksum.
There is no long-lived publishing secret in the repository.

### Register a publisher

Open **My packages → your package → GitHub Actions**. Enter a public repository
as `OWNER/REPO`, its workflow path, exact allowed refs/events, and an optional
GitHub environment. The server resolves and stores the numeric repository and
owner IDs. Repository names are display information; they do not establish trust.
Save takes effect immediately. The CLI equivalent is `apell package oidc set`;
see the [CLI connection commands](store-cli.md#connect-github-actions).

Every maintainer can change or disconnect this connection. Both changes invalidate
outstanding scoped credentials, including uploads still in progress. They do not
remove existing releases or change the applet ID. A repository rename under the
same owner continues to work; changing repositories or transferring repository
ownership requires saving the connection again. Removing a maintainer does not
automatically disconnect a separately trusted repository.

Each package has one publishing connection. Allowed refs are exact strings;
wildcards are not supported. Only GitHub-hosted runners are accepted. Pull-request
events are rejected. A publishing job must be defined in the registered workflow;
delegation to a different reusable workflow is not supported by this adapter.

Existing operator-managed JSON policies are imported once during migration; see
[operator setup](store-accounts.md#operator-setup). Subsequent edits use the website
or CLI and are saved in the database, so a restart cannot restore a connection
that was disconnected.

### Configure the workflow

Set the non-secret repository variable `APELL_STORE_URL` to the canonical Store
origin. Use `agentic-rs/apell/actions/publish` in a job defined directly in your
registered workflow. It installs the Apell CLI from the action's revision, then
validates and publishes your built `.apel`, listing JSON, and optional PNG icon.
The publishing job needs `id-token: write`.

Keep the build in a separate job with `contents: read` so frontend dependencies
and build scripts run without publishing access. If the build needs the CLI,
`agentic-rs/apell/actions/setup` installs `apell` on `PATH`. Both actions use Bash
and Rustup, which are available on the GitHub-hosted Linux runners shown below.

Save a workflow like this as `.github/workflows/publish.yml`. Pin both actions to
the same `actions-vMAJOR.MINOR.PATCH` tag. These tags version the actions
independently of the Apell shell. Each published version keeps its original
commit. If your repository requires full SHA pins, use the commit for the tag.

Adapt the build command and filenames to your project. This example assumes
your build prepares `dist/` with `manifest.json`, `index.html`, and any assets
or WASM, and provides `publish.json` at the repository root.

```yaml
name: Publish applet

on:
  workflow_dispatch:

permissions: {}

concurrency:
  group: apell-store-publish
  cancel-in-progress: false

jobs:
  build:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    outputs:
      artifact_id: ${{ steps.release.outputs.artifact-id }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7
        with:
          persist-credentials: false
      - uses: agentic-rs/apell/actions/setup@actions-v1.0.0
      - name: Build the applet and prepare its listing
        run: |
          ./scripts/build-applet
          apell pack dist --output release/example.apel
          cp publish.json release/
      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2
        id: release
        with:
          name: applet-store-${{ github.run_attempt }}
          path: release/
          if-no-files-found: error
          retention-days: 7
  publish:
    needs: build
    runs-on: ubuntu-latest
    permissions:
      id-token: write
    steps:
      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2
        with:
          artifact-ids: ${{ needs.build.outputs.artifact_id }}
          path: release
          digest-mismatch: error
      - uses: agentic-rs/apell/actions/publish@actions-v1.0.0
        with:
          store_url: ${{ vars.APELL_STORE_URL }}
          package: release/example.apel
          metadata: release/publish.json
```

In Store settings, register your repository's `.github/workflows/publish.yml`
with the `refs/heads/main` ref and the `workflow_dispatch` event for this example.
If the Store connection requires a GitHub environment, set `environment` on the
publish job. To include an icon, add its PNG to the uploaded artifact and pass
its downloaded path as the `icon` input.

The build sets the release channel through the version in `manifest.json`.
`1.2.0` publishes to Stable and `1.3.0-nightly.1` to Nightly. Give each changed
release a new version. The [Companion workflow](../.github/workflows/publish-companion.yml)
uses the same action and also publishes from `main` on manifest changes.

The CLI selects OIDC automatically in GitHub Actions. `--auth github-oidc` makes
that requirement explicit; a missing identity token or failed exchange never
falls back to the broad local token. `--auth token` explicitly selects the local
token-file flow. Identity and publishing tokens remain in memory and are not
written to disk or printed.

The store verifies GitHub's RS256 signature, issuer, audience, time claims, numeric
repository/owner IDs, and workflow/ref/event/environment claims. It uses GitHub's
fixed key endpoint with bounded caching and refreshes. Within a running server, each identity token can be
exchanged once; replay tracking remains until that identity expires. Scoped
publishing credentials last at most five minutes and never outlive the identity
token. They permit retries of the same package while valid. Both the parsed applet
ID and actual package checksum are checked before publication. Restarting the
store invalidates outstanding scoped credentials and clears the in-memory replay cache.

GitHub may supply `job_workflow_ref` for a direct job. When present, it must exactly
match the authorized `workflow_ref`; its presence alone does not make the job a
delegated workflow.

This establishes which workflow may publish. It does not inspect applet behavior
or replace runtime permission controls. Protect the authorized branch and workflow
according to who should be able to release your applet. See GitHub's
[OIDC reference](https://docs.github.com/en/actions/reference/security/oidc) for the
claims and job permissions.

For a rejected identity, the operator can temporarily set
`APELL_STORE_OIDC_DIAGNOSTICS=1` and restart the store. Service logs then report
fixed failure categories for token verification and policy matching. They do not
include tokens, claim values, repository names, or actors. Remove the setting and
restart after troubleshooting. Client-facing authentication errors stay generic.

## Release rules

- A published applet ID and version identify immutable content and metadata.
- Publishing the same package and metadata again succeeds without changing the release.
- Different package bytes, metadata, or icon at an existing version return
  `409 Conflict`; increment the version.
- Versions are ordered by SemVer precedence, not by upload time. A later upload of
  an older version cannot replace the recommended download.
- A version without a SemVer prerelease suffix (`1.2.0`) is **Stable**. Any
  SemVer prerelease (`1.3.0-nightly.2`, `1.3.0-beta.1`) is **Nightly**. The Store
  selects the latest version within each channel independently.
- Versions differing only by `+build` metadata cannot coexist. Use a distinct
  prerelease version such as `1.2.0-nightly.2` for each nightly build.

Apell's ZIP validation is shared by the shell, CLI, and store through
`apell-package`: 24 MiB archive size, 64 MiB expanded size, 1,024 entries, and a
64 KiB manifest. Invalid paths, colliding entries, encrypted archives, invalid CRCs,
and unsupported host API versions or permissions are rejected. Package code is
stored for download; it is never executed or served as a website by the store.

## HTTP API

All API responses use JSON with snake_case fields. URLs in catalog responses are
relative to the store origin.

| Method | Route                                              | Purpose                                                      |
| ------ | -------------------------------------------------- | ------------------------------------------------------------ |
| GET    | `/healthz`                                         | Process health                                               |
| GET    | `/api/v1/applets`                                  | Public catalog, including latest Stable and Nightly releases |
| GET    | `/api/v1/applets/{id}`                             | Listing and complete version history                         |
| GET    | `/api/v1/applets/{id}/versions/{version}/download` | Immutable `.apel` attachment                                 |
| POST   | `/api/v1/publish`                                  | Authenticated multipart upload                               |
| POST   | `/api/v1/auth/github-actions`                      | Exchange a trusted GitHub identity for a scoped upload token |

Publishing uses `Authorization: Bearer <token>` and multipart fields `package`
(`.apel` bytes), `metadata` (listing JSON), and optional `icon` (PNG bytes). A new
release returns `201`; an identical republish returns `200`:

```json
{
  "applet_id": "example",
  "version": "1.0.0",
  "sha256": "<64 lowercase hexadecimal characters>",
  "created": true
}
```

Errors have the shape `{"error":{"code":"…","message":"…"}}`. Publishing
authentication happens before the upload body is read. Requests and concurrent
validation are bounded. The small-store catalog supports up to 256 applets and
1,024 releases per applet; exceeding those limits is an explicit error.

The OIDC exchange takes `Authorization: Bearer <github-id-token>` and JSON
`{"applet_id":"example","sha256":"<package-sha256>"}`. Its response contains
`token` and `expires_at` (Unix seconds) and uses `Cache-Control: no-store`. Use
that token as the bearer credential on `/api/v1/publish`. The exchange is disabled
unless the public Store URL is configured. Only workflows registered in the package database can exchange an identity.

## Run locally

```sh
pnpm store:build
mkdir -p store/.local
(umask 077; openssl rand -hex 32 > store/.local/publish-token)
export APELL_STORE_PUBLISH_TOKEN_FILE=store/.local/publish-token
pnpm store:serve
```

Open `http://127.0.0.1:8788`. Generated credentials, data, and web build output are
ignored by Git. The token must contain 32–512 printable ASCII bytes without spaces.
Restart the service after changing it.

| Environment variable                  | Default                                           |
| ------------------------------------- | ------------------------------------------------- |
| `APELL_STORE_LISTEN`                  | `127.0.0.1:8788`                                  |
| `APELL_STORE_DATA_DIR`                | `store-data`                                      |
| `APELL_STORE_WEB_DIR`                 | `store/web/dist`                                  |
| `APELL_STORE_PUBLISH_TOKEN_FILE`      | Required                                          |
| `APELL_STORE_PUBLIC_URL`              | Unset; enables OIDC and supplies the login origin |
| `APELL_STORE_TRUSTED_PUBLISHERS_FILE` | Optional one-time legacy policy import            |
| `APELL_STORE_GITHUB_OAUTH_FILE`       | Optional private OAuth client JSON file           |
| `APELL_STORE_BOOTSTRAP_OWNER_FILE`    | Optional migration-only owner identity JSON file  |
| `APELL_STORE_OIDC_DIAGNOSTICS`        | Unset; failure diagnostics disabled               |

## Deploy

Build `apell-store` for the server's operating system and architecture, and run
`pnpm store:build`. Deploy the binary and web build together. The service needs
write access only to its data directory and read access to the token and web files.
Place it behind HTTPS; the default listener is local-only.

Generic [systemd](../deploy/store/apell-store.service) and
[Nginx](../deploy/store/nginx.conf.example) templates are included. Replace the
example domain and certificate paths in a local copy. Keep actual host names,
addresses, and credentials out of Git. The systemd template uses a dedicated
service account, a persistent data directory, and a 384 MiB memory limit.

For trusted publishing, set the public Store URL in
`/etc/apell-store/environment`, using the
[environment example](../deploy/store/environment.example). The systemd unit
reads this optional file. Keep it and any OAuth/import files readable only by
the administrator and service account. The server needs outbound HTTPS access to
GitHub's OIDC key endpoint. No new inbound port is required.

Before deploying, create the service account and token file with restrictive
permissions. Point `/opt/apell-store/current` at a release directory containing
`apell-store` and `web/`. Test Nginx configuration before reloading it, then verify
`/healthz`, catalog browsing, unauthorized publishing rejection, and a package
download with a matching SHA-256.

Keep the HTTP ACME challenge route reachable for certificate renewal. Verify that
the system's Certbot renewal timer is enabled, and configure a deploy hook that
tests and reloads Nginx after a successful renewal. Run a renewal dry run for the
store certificate before completing deployment. The proxy read timeout must
exceed the server's 120-second upload deadline.

Back up the entire data directory with the service stopped, or use SQLite's online
backup API and copy the immutable content files. Preserve the token separately.
Keep the previous binary and web directory for rollback; deployment must not
replace or delete the store's persistent data.

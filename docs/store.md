# Apell Store

The store serves a public applet catalog, version history, and `.apel` downloads.
Publishing requires a private bearer token. Applets remain in their own repositories;
publishing a release does not require changing the shell or landing-page repository.

The website supports search, stable and preview releases, declared permissions,
release notes, and SHA-256 checksums. Download a package, then use **+ → Install an
.apel file** in Apell. Store browsing and automatic store updates inside the native
shell are not included yet. Existing file and GitHub installation still work.

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

Keep the publishing token in a private file outside source control. Set
`APELL_STORE_PUBLISH_TOKEN_FILE` to that file, then publish:

```sh
apell publish artifacts/example.apel \
  --store https://store.example.invalid \
  --metadata listing.json \
  --icon icon.png
```

Replace the example URL with your store. The optional icon is a still PNG, at most
256 KiB and 1,024 × 1,024 pixels. The CLI validates the package before sending it,
requires HTTPS except for local loopback development, and refuses redirects so the
publishing token cannot be forwarded to another destination. CI can write its secret
to a temporary private file and set the same environment variable. Do not put the
token in an argument, listing, package, or repository.

The server reads the ID, display name, version, permissions, and API version from
the package itself. Listing metadata is separate from `manifest.json`. Descriptions
are limited to 2,000 UTF-8 bytes and release notes to 10,000 bytes. Project links
must use HTTPS without URL credentials.

## Release rules

- A published applet ID and version identify immutable content and metadata.
- Publishing the same package and metadata again succeeds without changing the release.
- Different package bytes, metadata, or icon at an existing version return
  `409 Conflict`; increment the version.
- Versions are ordered by SemVer precedence, not by upload time. A later upload of
  an older version cannot replace the recommended download.
- The website recommends the newest stable release when one exists, otherwise the
  newest prerelease. Preview releases remain available in version history.
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

| Method | Route                                              | Purpose                                                    |
| ------ | -------------------------------------------------- | ---------------------------------------------------------- |
| GET    | `/healthz`                                         | Process health                                             |
| GET    | `/api/v1/applets`                                  | Public catalog, including latest and latest stable release |
| GET    | `/api/v1/applets/{id}`                             | Listing and complete version history                       |
| GET    | `/api/v1/applets/{id}/versions/{version}/download` | Immutable `.apel` attachment                               |
| POST   | `/api/v1/publish`                                  | Authenticated multipart upload                             |

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

| Environment variable             | Default          |
| -------------------------------- | ---------------- |
| `APELL_STORE_LISTEN`             | `127.0.0.1:8788` |
| `APELL_STORE_DATA_DIR`           | `store-data`     |
| `APELL_STORE_WEB_DIR`            | `store/web/dist` |
| `APELL_STORE_PUBLISH_TOKEN_FILE` | Required         |

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

Before deploying, create the service account and token file with restrictive
permissions. Point `/opt/apell-store/current` at a release directory containing
`apell-store` and `web/`. Test Nginx configuration before reloading it, then verify
`/healthz`, catalog browsing, unauthorized publishing rejection, and a package
download with a matching SHA-256.

Back up the entire data directory with the service stopped, or use SQLite's online
backup API and copy the immutable content files. Preserve the token separately.
Keep the previous binary and web directory for rollback; deployment must not
replace or delete the store's persistent data.

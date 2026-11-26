# Local Apell development

Run the Store and Account relay in Docker using a test identity and isolated
data. Vite runs on the host so web edits appear as you save. Native builds
continue to use the host's Xcode or Android toolchain.

## Start the services

Install Docker or Podman, OpenSSL, and the project's Node.js and pnpm versions.
The first container build downloads images and Rust dependencies.

```sh
pnpm install --frozen-lockfile
pnpm dev:local up
pnpm dev:local logs
```

Wait for `Apell Store is ready` and `Apell Account key exchange is ready`.
Pressing Control-C exits the log viewer. The services keep running.
Set `APELL_LOCAL_ENGINE=podman` for each `dev:local` command if your engine is
named `podman`.

```sh
pnpm dev:local web
```

Open [the local Store](https://localhost:8788). Trust the development CA in your
test browser using `deploy/local/.state/config/tls/ca.crt`. This is a per-checkout
CA for `localhost`. Configure your test client to trust that CA and leave TLS
verification enabled. The local Store offers a fictional publisher through
**Continue with Apell Account**. Choose **Approve** at the test issuer to sign
in. The release catalog starts empty.

For the [landing page](http://127.0.0.1:8877), use `pnpm landing:dev`. The
Chinese locale is at `/cn/`.

## Iterate

- Store frontend and landing page edits reload through Vite.
- Run `pnpm dev:local reload` after editing Rust or the test issuer. This
  refreshes the source snapshot and rebuilds the services with cached Cargo
  dependencies. It keeps the local Store and relay databases.
- Run `pnpm dev:local stop` to stop the container. Stop the host Vite server
  with Control-C. Restart with `pnpm dev:local up` and `pnpm dev:local web`.

The published ports bind to loopback. The container receives an allowlisted
source snapshot and generated local credentials. It uses a per-checkout Docker
data volume. Production configuration, credentials, and data stay outside the
stack. The test publisher belongs to a synthetic OIDC issuer. This workflow
exercises Apell's OAuth callbacks and relay integration with deterministic test
identity. Use a separate Logto environment to test real social sign-in,
passkeys, or email delivery.

To publish a test `.apel` from the host, use the generated local token from
`deploy/local/.state/config/publish-token` with the
[Store publishing API](store.md#http-api). To test maintainer or publishing
settings, register a package in the local Store first.

## Connect a native test app

Use a separate macOS user for local native testing so the test app gets its own
app data and Keychain. On iOS, use a separate test simulator. Trust the
generated CA there before running a local-profile test app. Port 5656 serves
the synthetic Account origin and the encrypted relay.

```sh
APELL_SHELL_BUILD_PROFILE="$(cat deploy/local/.state/config/shell.env)" \
  APELL_REQUIRE_KEY_SYNC=1 pnpm desktop:dev
```

With the local profile, sign in and approve the displayed device code at the
local test issuer. The same fictional account is available for testing first
verification, pairing from a second test install, and browser recovery. Use
fictional model settings and API keys for sync tests. If the fixture restarts
while sign-in or recovery is pending, start a fresh request.

These loopback URLs are intended for the host and the iOS Simulator. A physical
phone or Android emulator needs its own network and development certificate
configuration to reach the services.

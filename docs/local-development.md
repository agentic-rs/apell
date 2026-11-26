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
  dependencies. It keeps the local Store and relay databases and completed
  native test sign-ins. Test sign-ins expire after 24 hours.
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

## Run the macOS test app

With the container and `pnpm dev:local web` running, open another terminal:

```sh
pnpm dev:local shell
```

**Apell Local** runs the macOS shell with hot reload, its own app data, and the
local Store configured by default. Running `shell` without a device name uses
the `default` profile in this checkout. Its mock credential store saves Account
sessions, provider keys, and applet secrets in owner-only files across app
restarts. WebView data is temporary. If you used an unnamed profile on an
earlier local build, sign in again and add your test connections to the new
profile. Native connections to the Store, Account, and encrypted relay trust
the generated development CA. Open device approval and recovery pages in a
test browser that trusts this CA.

The fictional account can test first verification and encrypted API key sync.
Use fictional model settings and API keys. If the fixture restarts while a
sign-in or recovery is pending, start a fresh request.

To test pairing between two macOS instances, start them in separate terminals
while the same local services are running:

```sh
pnpm dev:local shell device-a
pnpm dev:local peer device-b
```

The peer shares the first shell's frontend server. Restart it after Rust changes.
Each device profile has its own app files and mock credentials under
`deploy/local/.state/devices/`. The devices use the same fictional Account.
Use different names for simultaneously running instances.

For a fresh local vault, sign both devices in, then save a fictional API key
and type a model ID on the first device. Select that connection, enable model
settings sync, and verify the device. Enable model settings sync on the peer
first, then pair it using the matching six-digit codes. After approval, the
peer should become Verified and select the synced model when its key arrives.

## Test on iOS

Use a separate test simulator with the generated CA installed and trusted.
Build the shell for that simulator with the local profile:

```sh
APELL_SHELL_BUILD_PROFILE="$(cat deploy/local/.state/config/shell.env)" \
  APELL_REQUIRE_KEY_SYNC=1 pnpm ios:dev 'Your test simulator name'
```

The simulator can act as a second device for pairing. A physical phone or
Android emulator needs its own network and development certificate
configuration to reach the services.

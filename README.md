# Apell

Apell is a personal applet host targeting iOS and Android, with a macOS development shell. Install your own applet bundles, keep their data in native SQLite, and share one LLM connection without passing provider keys into applets.

The included **Companion** chatbot exercises the whole boundary: a TypeScript interface calls Rust WASM, which owns conversation logic and SQL migrations, then uses asynchronous host APIs for SQLite and LLM requests. The installed Tauri runtime stays native.

## First run

Install a current stable Rust toolchain, Node.js 24 or newer, pnpm, and the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/). Node's built-in SQLite module is used by the WASM integration test.

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126 --locked
pnpm install --frozen-lockfile
pnpm desktop:dev
```

The `wasm-bindgen` CLI version must match the chatbot's pinned Rust dependency. Development and production frontend builds compile the chatbot automatically.

In the shell:

1. Select **Install sample**, then open **Companion**.
2. Start a conversation. The initial demo connection produces scripted replies locally, allowing streaming, cancellation, and persistence to be exercised without credentials.
3. Open **Model connection** to configure an OpenAI-compatible API base URL, model ID, and optional key. The host appends `/chat/completions` to the base URL. Keys remain in native secure storage.

Closing and reopening the applet preserves conversations. A stopped reply is marked interrupted; interrupted replies are excluded from later model context. A pending reply left by an applet shutdown is marked interrupted when the applet next initializes.

`pnpm dev` alone serves the frontend in a browser. SQLite, applet execution, and credentials require the native shell.

## Build and test

```sh
pnpm build
pnpm test
cargo test -p apell-core
cargo test --manifest-path examples/chatbot/Cargo.toml
cargo check -p apell
pnpm desktop:build
```

`pnpm test` builds and executes the actual chatbot WASM against a SQLite-backed host substitute, and tests the bridge request contract. The Rust core tests cover native SQLite isolation and transactions, bundle integrity and concurrent installation, resources, HTTP transport, applet files and config, and LLM parsing and cancellation. These checks complement device testing; they do not establish mobile packaging, signing, or runtime behavior.

## Mobile development

iOS requires macOS with Xcode and its simulator/device tools. Android requires the Android SDK, NDK, Java tooling, and an emulator or connected device. The credential plugin compiles against Android SDK 36 and has a minimum API level of 24. Keep SDK locations and signing configuration local.

```sh
# Generate the native project on the development machine.
pnpm ios:init
pnpm ios:dev

# Or initialize and run Android.
pnpm android:init
pnpm android:dev
```

Build commands are available through the local Tauri CLI:

```sh
pnpm tauri ios build --debug --target aarch64-sim --no-sign
pnpm tauri android build --debug --target aarch64
```

Choose a target supported by the local toolchain. Physical iOS devices require local signing setup. Generated Xcode and Gradle projects under `src-tauri/gen/` are ignored because they can contain machine paths and signing identities; regenerate them with the init commands.

## Applet bundles

```sh
pnpm build:chatbot
```

This produces `artifacts/chatbot.apell.json` and `artifacts/chatbot.sha256`. The bundle is JSON containing a versioned manifest and a self-contained HTML document. CSS, JavaScript, and the compiled WASM bytes are embedded; installation does not need a separate asset server.

Use **Choose bundle** for a local file. To download a bundle, provide its URL and the SHA-256 from a trusted copy of the build output. Downloads require HTTPS, with loopback HTTP allowed for local development. Remote redirects are not followed. No hosting or publication is performed by this project.

A checksum pins exact bytes; it is not a publisher signature. This version is for applets you build and trust. Installing another bundle with the same `applet_id` updates its code while retaining its local databases, resources, files, and config.

## Current scope

- One foreground applet with a restricted message bridge.
- Native SQLite with applet-owned schemas and SQL, typed parameters, and transaction handles.
- Shared LLM configuration, streaming text completions, and cancellation.
- Provider keys in Apple Keychain or Android Keystore-backed encrypted storage.
- Applet-scoped resource downloads and bounded binary reads.
- Native HTTP requests with binary bodies, headers, timeouts, and session cancellation.
- Applet-private file operations and persistent JSON configuration.
- A basic `device.info` API for platform and host API version.

Cloud synchronization, tool calling, model discovery, generic per-applet credential APIs, background applets, and additional device integrations are deferred. Version files are retained, but there is no rollback selector, uninstall flow, or automatic resource eviction yet. Reinstall a known bundle to select its code version; database downgrade compatibility remains the applet's responsibility.

See [the architecture and API contract](docs/architecture.md) for migration guidance and limits, and [verification notes](docs/verification.md) for tested behavior.

## Repository hygiene

Commit source, lockfiles, and generated application icons. Keep build products, bundled WASM output, downloadable artifacts, generated platform projects, local environment files, and logs out of Git. Use a generic repository-local author identity for automated commits. Run `pnpm check:staged` and review staged content for credentials, usernames, machine-specific paths, hostnames, and signing identities before every commit. Publishing and pushing require a separate instruction.

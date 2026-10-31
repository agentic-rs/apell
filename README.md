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

1. Tap **+**, choose **Try Companion**, then tap the **Companion** icon.
2. Start a conversation. The initial demo connection produces scripted replies locally, allowing streaming, cancellation, and persistence to be exercised without credentials.
3. Open **Settings → Model connection** to configure an OpenAI-compatible API base URL, model ID, and optional key. The host uses the embedded [Tokn SDK](https://github.com/tokn-ai/tokn) and appends `/chat/completions` to the base URL. Keys remain in native secure storage.

The library shows each applet as one icon and its name. Add and Settings stay in the header. To refresh the bundled sample after updating the shell, choose **+ → Update Companion**; existing chats are preserved.

Inside an applet, the top-right capsule contains **…** and **Close**. The menu offers **Reload applet** and **About this applet**. Reload starts a fresh session and keeps saved data; unsaved input is reset. About shows the applet's version and requested access. Close returns to the library.

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

For installation from Xcode, run `pnpm tauri ios build --debug --open`, keep that terminal running, select the `apell_iOS` target, enable automatic signing with your development team, select the connected iPhone, and Run. This builds a bundled frontend that works without the development server once installed.

iOS build/dev commands automatically run `pnpm ios:prepare`. It prepares the generated Xcode Rust build phase to find Node, pnpm, and Rust even when Xcode was opened from the Dock. Tool locations are resolved from the current terminal into an ignored local environment file; personal paths and signing settings stay out of source files. After changing Node or Rust installations, rerun the Tauri command or `pnpm ios:prepare`. If Xcode reports `pnpm: command not found`, run the prepare command and retry the build. Keep the Tauri terminal running during Xcode builds so its build-options service remains available.

## Applet bundles

```sh
pnpm build:chatbot
```

This produces `artifacts/chatbot.apell.json` and `artifacts/chatbot.sha256`. The bundle is JSON containing a versioned manifest and a self-contained HTML document. CSS, JavaScript, and the compiled WASM bytes are embedded; installation does not need a separate asset server.

Tap **+ → Choose a bundle** for a local file. Use **Install from a URL** to provide a URL and the SHA-256 from a trusted copy of the build output. Downloads require HTTPS, with loopback HTTP allowed for local development. Remote redirects are not followed. No hosting or publication is performed by this project.

A checksum pins exact bytes; it is not a publisher signature. This version is for applets you build and trust. Installing another bundle with the same `applet_id` updates its code while retaining its local databases, resources, files, and config.

## Current scope

- One foreground applet with a restricted message bridge.
- Native SQLite with applet-owned schemas and SQL, typed parameters, and transaction handles.
- Shared LLM configuration, tool calling, separate answer/reasoning events, and cancellation.
- Opaque session continuation, explicit generation outcomes, and per-request model attribution.
- Provider keys in Apple Keychain or Android Keystore-backed encrypted storage.
- Applet-scoped resource downloads and bounded binary reads.
- Native HTTP requests with binary bodies, headers, timeouts, and session cancellation.
- Applet-private file operations and persistent JSON configuration.
- A basic `device.info` API for platform and host API version.

Cloud synchronization, provider model catalogs, schema-constrained answers, generic per-applet credential APIs, background applets, and additional device integrations are deferred. The current Tokn integration uses OpenAI-compatible Chat Completions; additional Tokn provider adapters, OAuth onboarding, and native Anthropic/Gemini protocols are not exposed by Apell. Version files are retained, but there is no rollback selector, uninstall flow, or automatic resource eviction yet. Reinstall a known bundle to select its code version; database downgrade compatibility remains the applet's responsibility.

## Tokn development dependency

Apell consumes `tokn-sdk` directly from Tokn's Git `main` branch. `Cargo.lock` records the exact revision so device builds stay reproducible. To dogfood the latest upstream changes, run `cargo update -p tokn-sdk`, run the checks and mobile build, then commit the updated lockfile. A normal locked build does not fetch a newer revision automatically.

The SDK runs inside the native host with an in-memory configuration. No Tokn gateway process or separate Tokn configuration/auth files are required. See [the integration boundary](docs/architecture.md#shared-llm-access) for the current protocol and continuation limits.

See [the architecture and API contract](docs/architecture.md) for migration guidance and limits, and [verification notes](docs/verification.md) for tested behavior.

New applets should use `llm.generate` and inspect its terminal status. See the [LLM tool continuation example](docs/llm.md). The legacy `llm.complete` text-only contract remains available for installed bundles.

## Repository hygiene

Commit source, lockfiles, and generated application icons. Keep build products, bundled WASM output, downloadable artifacts, generated platform projects, local environment files, and logs out of Git. Use a generic repository-local author identity for automated commits. Run `pnpm check:staged` and review staged content for credentials, usernames, machine-specific paths, hostnames, and signing identities before every commit. Publishing and pushing require a separate instruction.

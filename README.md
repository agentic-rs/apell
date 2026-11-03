# Apell

Apell is a personal Tauri shell for iOS and Android, with a macOS development host. Install a `.apel` package from a file or a public GitHub release. Its frontend runs in a sandboxed web view. An optional Rust WebAssembly backend runs in the native shell through the Wasmi interpreter. Applets use host APIs for SQLite, files, resources, HTTP, configuration, device information, and shared model access.

The included **Companion** chatbot keeps conversations, SQL migrations, model targets, and in-flight replies in its Rust backend. The interface can close or reload while the backend completes a reply. Reopening the interface asks the backend for its current state. Saved chats survive an Apell restart.

## First run

Install a current stable Rust toolchain, Node.js 24 or newer, pnpm, and the [Tauri platform prerequisites](https://v2.tauri.app/start/prerequisites/).

```sh
rustup target add wasm32-unknown-unknown
pnpm install --frozen-lockfile
pnpm desktop:dev
```

The development and production builds compile Companion's Rust guest for `wasm32-unknown-unknown`, bundle its TypeScript and CSS, and pack the result with the Rust CLI. Rust emits the guest module directly.

1. Open **Settings**, choose a provider, enter its API key or complete sign-in, select a model, then **Save connection**.
2. Tap **+ → Try Companion** and open its icon. Use Companion's model button to choose the provider and model for a chat. Provider credentials stay in Apell's native credential store.
3. Send a message. Use **… → Reload applet** during a reply, or close Companion and reopen its icon. Both actions attach a fresh interface to the same running backend. **About this applet** shows the version and declared permissions.

`pnpm dev` serves the shell frontend for development. Launch the native Tauri application to install and run applets.

## Build and test

```sh
pnpm build
pnpm test
cargo test --workspace --locked
pnpm desktop:build
```

The TypeScript suite exercises the SDK and shell bridge. The Rust suites cover package integrity, source-bound updates, native services, guest execution, and run/view isolation. The runtime integration tests compile and execute Companion in Wasmi with native SQLite and controlled model responses. See the [verification notes](docs/verification.md) for platform checks.

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
pnpm tauri ios build --debug --target aarch64-sim
pnpm tauri android build --debug --target aarch64
```

Choose a target supported by the local toolchain. Physical iOS devices require local signing setup. Generated Xcode and Gradle projects under `src-tauri/gen/` are ignored because they can contain machine paths and signing identities; regenerate them with the init commands.

After each completed app change, update the running simulator as well as building it. On Apple Silicon with one simulator booted:

```sh
pnpm tauri ios build --debug --target aarch64-sim --ci
xcrun simctl install booted src-tauri/gen/apple/build/arm64-sim/Apell.app
xcrun simctl launch --terminate-running-process booted dev.apell.shell
```

Verify the installed executable matches the build before reporting the simulator updated. Reinstalling the shell retains applet data. If the bundled Companion changed, choose **+ → Update Companion**. If Companion had already started, restart Apell before checking the new version.

Keep signing enabled for runnable simulator builds. Xcode's local simulator signing embeds the app identity required by Keychain; `--no-sign` is suitable only for compile checks and cannot verify provider credentials or sign-in.

For installation from Xcode, run `pnpm tauri ios build --debug --open`, keep that terminal running, select the `apell_iOS` target, enable automatic signing with your development team, select the connected iPhone, and Run. This builds a bundled frontend that works without the development server once installed.

iOS build/dev commands automatically run `pnpm ios:prepare`. It prepares the generated Xcode Rust build phase to find Node, pnpm, and Rust even when Xcode was opened from the Dock. Tool locations are resolved from the current terminal into an ignored local environment file; personal paths and signing settings stay out of source files. After changing Node or Rust installations, rerun the Tauri command or `pnpm ios:prepare`. If Xcode reports `pnpm: command not found`, run the prepare command and retry the build. Keep the Tauri terminal running during Xcode builds so its build-options service remains available.

## Create and distribute an applet

An applet can live in its own repository. Stage the built frontend and optional backend in one directory. Use relative asset URLs so the frontend loads correctly from its view-specific package URL.

```text
stage/
  manifest.json
  index.html
  assets/main.js
  assets/main.css
  lib/main.wasm
```

```json
{
  "schema_version": 1,
  "api_version": 2,
  "applet_id": "example",
  "name": "Example",
  "version": "1.0.0",
  "permissions": ["sqlite", "llm"],
  "lib": "lib/main.wasm"
}
```

For a frontend-only applet, omit `lib`. Keep `applet_id` stable across releases so the applet retains its databases, files, resources, and preferences. Use a semantic version for each release.

To use the SDKs from a separate repository, keep a pinned checkout of Apell available to the build. With that checkout under `vendor/apell`, run these commands from the applet repository.

```sh
pnpm add ./vendor/apell/packages/applet-sdk
cargo install --locked --path vendor/apell/crates/apell-cli
```

Import `@apell/sdk` in the frontend and bundle it with the applet. For a Rust backend, add these entries to the backend's `Cargo.toml`, adjusting the path relative to that file.

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
apell-guest = { path = "vendor/apell/crates/apell-guest" }
serde_json = "1"
```

Build with `cargo build --release --target wasm32-unknown-unknown` and copy the resulting `.wasm` file into `stage/` at the manifest's `lib` path. Companion’s build script remaps compiler source paths before packaging its guest for distribution. The guest SDK exports the ABI used by Wasmi. A minimal command handler is documented in the [architecture guide](docs/architecture.md#rust-backend).

```sh
apell pack stage --output artifacts/example.apel
```

The output must be outside the input directory. The CLI writes a deterministic ZIP archive and prints its SHA-256. From the Apell checkout, `pnpm build:chatbot` produces `artifacts/chatbot.apel` and `artifacts/chatbot.sha256` for Companion 0.4.0.

Install a local `.apel` file from the **+** sheet. For GitHub distribution, attach the `.apel` file to a published stable release in a public applet repository. Use the same asset name for later releases, such as `example.apel`. In Apell, choose **Install from GitHub**, enter `owner/repository`, find the release, and choose its package if several are available. The package contains its own manifest. The host checks GitHub's asset digest when available and computes its own content hash. Only install packages from developers you trust. Publisher signatures are outside the current format.

A GitHub install records the numeric repository ID, repository name, stable asset name, and applet ID. **+ → GitHub updates → Update** checks that binding before switching versions. A file install accepts updates from a file. Installing changed local content over a GitHub applet clears its GitHub update source. Versions cannot decrease, and a GitHub release with changed bytes must advance the applet version. An identical reinstall keeps the existing source.

An update makes the new package available for the next run. If the applet already has a run in this Apell process, it keeps its current code and assets across close, reopen, and reload. Restart Apell to start that applet with the updated package. Previously installed API-1 JSON applets can still load from their existing cache. New installs require `.apel` packages.

## Current scope

Apell supports one attached view and up to eight applet runs per process. Backends can continue work without a view while the host process runs. A mobile OS may suspend or terminate the process. Persist state that must survive termination in the host's SQLite, file, or config service. Cloud synchronization, automatic resource eviction, an uninstall flow, and a rollback selector are deferred.

Applets own their SQLite schema and migrations and issue asynchronous SQL calls through the native service. Each applet has a private file area, downloadable resources, and non-secret JSON preferences. Credentials and provider settings are managed by the shell.

Settings supports saved named connections, model discovery and manual IDs, API keys, and Codex/GitHub Copilot device sign-in. **Use connection** selects the shared default. The applet's model picker can select a different target for one conversation. Companion starts a new conversation when changing the target of a populated chat. **Test connection** sends a short model request.

The native host uses the embedded Tokn SDK for provider routing. Supported connections include OpenAI, Codex, GitHub Copilot, DeepSeek, OpenCode Go, llama.cpp, Z.ai/Zhipu and their coding plans, and custom OpenAI-compatible endpoints. Applets keep their complete assistant messages and inspect the returned generation status. See the [provider settings contract](docs/architecture.md#provider-settings) and [LLM history and tool example](docs/llm.md).

## Tokn development dependency

Apell consumes `tokn-sdk` directly from Tokn's Git `main` branch. `Cargo.lock` records the exact revision so device builds stay reproducible. To dogfood the latest upstream changes, run `cargo update -p tokn-sdk`, run the checks and mobile build, then commit the updated lockfile. A normal locked build does not fetch a newer revision automatically.

The SDK runs inside the native host with an in-memory configuration. No Tokn gateway process or separate Tokn configuration/auth files are required. See [the integration boundary](docs/architecture.md#shared-llm-access) for the current protocol and continuation limits.

See [the architecture and API contract](docs/architecture.md) for migration guidance and limits, and [verification notes](docs/verification.md) for tested behavior.

## Repository hygiene

Commit source, lockfiles, and generated application icons. Keep build products, bundled WASM output, downloadable artifacts, generated platform projects, local environment files, and logs out of Git. Use a generic repository-local author identity for automated commits. Run `pnpm check:staged` and review staged content for credentials, usernames, machine-specific paths, hostnames, and signing identities before every commit. Publishing and pushing require a separate instruction.

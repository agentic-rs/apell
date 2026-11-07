# Apell

Apell is a personal Tauri shell for iOS, Android, and macOS. Install a `.apel` package from a file or a public GitHub release. Its frontend runs in a sandboxed web view. An optional Rust WebAssembly backend runs in the native shell through the Wasmi interpreter. Applets use host APIs for SQLite, files, resources, HTTP, configuration, device information, and shared model access.

The included **Companion** chatbot keeps conversations, SQL migrations, model targets, and in-flight replies in its Rust backend. The interface can close or reload while the backend completes a reply. Reopening the interface asks the backend for its current state. Saved chats survive an Apell restart.

Use the conversation drawer to search saved chats and start a new one. Switching chats keeps each draft and reading position while the interface stays open. The compact composer has an attachment **+** menu and a reply format selector.

Companion can attach a selected photo as JPEG and import WAV/MP3 audio files on iOS, Android, and macOS. On macOS, **Photo** opens an image file picker. Choose **Image** or **Audio** in the composer's reply format selector to request generated media from a compatible model. Audio replies use a provider voice ID. Completed images appear in the conversation, and audio has Play/Pause controls. Attachments and generated files stay with the saved chat.

Building an applet? [View the Apell Applet skill](skills/apell-applet/SKILL.md) or [install it in Codex](#applet-authoring-skill).

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

## macOS app

The native macOS app starts with a 420×820 content area and the same launcher, settings, and applet controls as mobile. Resize the window to use the existing wider layouts, including Companion's conversation sidebar. Closing the window keeps Apell and its applet backends running; reopening Apell from the Dock restores the window. Use **Apell → Quit Apell** to stop the host.

Build and open the standalone application on macOS 11 or later:

```sh
pnpm desktop:build --bundles app
open target/release/bundle/macos/Apell.app
```

The `.app` contains the frontend and Companion bundle and runs without a development server. The macOS configuration uses an ad-hoc signature for local use. Configure a Developer ID locally for public distribution, following [Tauri's macOS signing guide](https://v2.tauri.app/distribute/sign/macos/). For an interactive development build, use `pnpm desktop:dev`. Build products and personal signing configuration stay outside Git.

macOS uses Keychain for provider credentials and native file dialogs for packages and encrypted settings transfers. Applets can import images and audio into their private files and play private audio through the same bridge used on mobile. Camera capture, microphone recording, location, QR transfer, and audio sharing remain mobile features; query audio capabilities before offering those controls. Windows and Linux do not yet have the credential and native media support required for this desktop release.

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

### Applet authoring skill

The [Apell Applet skill](skills/apell-applet/SKILL.md) guides agents through packaging and APIs, mobile architecture, and interaction review. It loads the references that match the task.

To install it, paste this into Codex.

```text
Use $skill-installer to install https://github.com/agentic-rs/apell/tree/main/skills/apell-applet
```

In a new turn in your applet repository, use `$apell-applet` with your task and the Apell revision you target. For a project-local installation, copy the whole [skill folder](skills/apell-applet) into `.agents/skills/` in the downstream repository.

### Package and distribute

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

The output must be outside the input directory. The CLI writes a deterministic ZIP archive and prints its SHA-256. From the Apell checkout, `pnpm build:chatbot` produces `artifacts/chatbot.apel` and `artifacts/chatbot.sha256` for Companion 0.6.1.

Install a local `.apel` file from the **+** sheet. For GitHub distribution, attach the `.apel` file to a published release in a public applet repository. Use the same asset name for later releases, such as `example.apel`. In Apell, choose **Install from GitHub**, enter `owner/repository` for the latest stable release, and choose its package if several are available. To install a nightly or another named release, enter `owner/repository@nightly` or paste its `https://github.com/owner/repository/releases/tag/nightly` URL. Explicit release selections accept published prereleases; repository-only lookup stays on stable releases. The package contains its own manifest. The host checks GitHub's asset digest when available and computes its own content hash. Only install packages from developers you trust. Publisher signatures are outside the current format.

A GitHub install records the numeric repository ID, repository name, stable asset name, applet ID, and explicit tag when selected. **+ → GitHub updates → Update** follows that saved tag or latest stable selection and checks its binding before switching versions. A file install accepts updates from a file. Installing changed local content over a GitHub applet clears its GitHub update source. Versions cannot decrease. Stable releases with changed bytes must advance the applet version; an already installed tagged prerelease can accept rebuilt content at the same version while it remains a prerelease. An identical reinstall keeps the existing source.

An update makes the new package available for the next run. If the applet already has a run in this Apell process, it keeps its current code and assets across close, reopen, and reload. Restart Apell to start that applet with the updated package. Previously installed API-1 JSON applets can still load from their existing cache. New installs require `.apel` packages.

## Current scope

Apell supports one attached view and up to eight applet runs per process. Backends can continue work without a view while the host process runs. A mobile OS may suspend or terminate the process. Persist state that must survive termination in the host's SQLite, file, or config service. Cloud synchronization, automatic resource eviction, an uninstall flow, and a rollback selector are deferred.

Applets own their SQLite schema and migrations and issue asynchronous SQL calls through the native service. Each applet has a private file area, downloadable resources, and non-secret JSON preferences. Rust backends can additionally declare generic binary WebSocket, applet-scoped secure-secret, secure-randomness and timer permissions; see [backend services](docs/architecture.md#backend-networking-and-secure-state). Shared provider credentials and provider settings remain managed by the shell.

Settings supports saved named connections, model discovery and manual IDs, API keys, and Codex/GitHub Copilot device sign-in. **Use connection** selects the shared default. The applet's model picker can select a different target for one conversation. Companion starts a new conversation when changing the target of a populated chat. **Test connection** sends a short model request.

The native host uses the embedded Tokn SDK for provider routing. Supported connections include OpenAI, Codex, GitHub Copilot, DeepSeek, OpenCode Go, llama.cpp, Z.ai/Zhipu and their coding plans, and custom OpenAI-compatible endpoints. Applets keep their complete assistant messages and inspect the returned generation status. See the [provider settings contract](docs/architecture.md#provider-settings) and [LLM history and tool example](docs/llm.md).

Applets can send private images, PDFs, or WAV/MP3 audio as user message attachments and request generated images or speech as private files. See the [media input](docs/architecture.md#media-input) and [media output](docs/architecture.md#media-output) contracts for permissions, formats, and limits.

## Recorder applet services

The host supports audio import and playback on iOS, Android, and macOS through the `audio` permission. Recording and file sharing are available on iOS and Android. The `speech` permission provides offline Whisper transcription and language detection after an explicit multilingual model download. Audio stays on the device. See [the audio and speech contract](docs/architecture.md#audio-and-offline-speech) for methods, formats, lifecycle, and limits.

Applets can take a still photo, import one selected photo, or request a foreground location fix on iOS and Android. macOS supports selecting an image file through the same photo API. See [the camera, photo library, and location contract](docs/architecture.md#camera-photo-library-and-location) for SDK calls and permission behavior.

Native speech compilation additionally requires CMake and a C/C++ toolchain. For Android builds, `pnpm tauri` also makes `NDK_HOME` available to Whisper's CMake build. Models and recordings live in application data and are never bundled into source control.

## Tokn development dependency

Apell consumes `tokn-sdk` directly from Tokn's Git `main` branch. `Cargo.lock` records the exact revision so device builds stay reproducible. To dogfood the latest upstream changes, run `cargo update -p tokn-sdk`, run the checks and mobile build, then commit the updated lockfile. A normal locked build does not fetch a newer revision automatically.

The SDK runs inside the native host with an in-memory configuration. No Tokn gateway process or separate Tokn configuration/auth files are required. See [the integration boundary](docs/architecture.md#shared-llm-access) for the current protocol and continuation limits.

See [the architecture and API contract](docs/architecture.md) for migration guidance and limits, and [verification notes](docs/verification.md) for tested behavior.

## Repository hygiene

Commit source, lockfiles, and generated application icons. Keep build products, bundled WASM output, downloadable artifacts, generated platform projects, local environment files, and logs out of Git. Use a generic repository-local author identity for automated commits. Run `pnpm check:staged` and review staged content for credentials, usernames, machine-specific paths, hostnames, and signing identities before every commit. Publishing and pushing require a separate instruction.

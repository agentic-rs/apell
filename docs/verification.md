# Verification

Companion 0.3.0 adds a host provider/model picker and conversation-owned routing. All 54 frontend/SDK/WASM tests and 132 host/core Rust tests passed, along with TypeScript checking, strict Clippy, formatting, the signed iOS simulator build, and the Android native check. The Android check passed when rerun separately after a generated Tauri plugin directory collision during parallel builds.

Fixtures cover picker permission and lifecycle boundaries, cancellation and stale responses, public selection projection, interactive SDK requests beyond the ordinary request timeout, explicit connection/model routing, invalid or removed targets, and preservation of requested provenance. Actual SQLite/WASM fixtures cover schema 1/2 migration, route persistence after reopen, safe legacy inference, and starting a fresh conversation when either target field changes.

The simulator shell executable matched the build output. Companion was updated separately through the same `BundleStore::install` API used by the shell, and its installed manifest and checksum matched the built 0.3.0 bundle. This changed only the bundle version pointer and preserved applet data. The shell was relaunched afterward. Simulator UI automation could inspect the launcher but failed to click it with `noWindowsAvailable`, so this update's picker flow was not smoke-tested in the simulator. Real provider authorization and model generation were not exercised. Local HTTP fixtures establish request routing and response handling.

Provider setup now supports authentication and model selection before Save. The update passed all 48 frontend/SDK/WASM tests and 130 native tests, TypeScript checking, strict Clippy, formatting checks, the signed iOS simulator build, and the Android native check. Draft regressions cover RAM-only OAuth, request-local discovery keys, failed-save rollback and retry, inherited credential references, destination isolation, expiry, late responses, and Save/discard ordering. Applets remain unable to call the draft administration operations.

The final simulator executable matched the build output. A local HTTP fixture returned two models to an unsaved compatible connection; selecting a model enabled Save, and leaving setup returned to the unchanged connection list. A new Codex setup showed Sign in enabled before saving. No provider account credentials were entered, no live OAuth authorization or live model request was exercised, and no fixture connection was persisted. The verification notes below describe earlier flows, including the superseded requirement to save before authentication.

The credential-form simplification passed all 44 frontend/SDK/WASM tests, TypeScript checking, formatting checks, and the signed iOS simulator build. Regression cases cover clearing a replacement key, undoing removal, preserving an unavailable credential, provider changes, and optional-key entry. Simulator smoke checks verified direct required and optional key fields without a credential-action dropdown. The rebuilt app was installed and relaunched, and its executable matched the build output. No credentials were entered or provider requests made for this check.

Provider connections and device sign-in use Tokn Git `main` at `552dfd60ee9bc333118f11fdc1bc3ad951c32b7e`. The update was checked with:

- `pnpm test`: all 43 SDK, bridge, compiled-WASM/SQLite, provider-settings, JSON validation, and iOS preparation tests passed.
- `cargo test -p apell-core -p apell --offline --locked`: all 123 native tests passed, including 25 loopback LLM integration tests.
- `cargo clippy --offline --locked -p apell-core -p apell --all-targets -- -D warnings`: passed.
- `pnpm tauri ios build --debug --target aarch64-sim --ci`: the bundled frontend, WASM sample, and native simulator app built successfully with local simulator signing.
- `cargo check --offline --locked -p apell --lib --target aarch64-linux-android`: passed with the installed NDK toolchain.
- `pnpm format:check`, `git diff --check`, and the staged privacy scan passed.

Fixtures cover provider-specific requests and headers, Responses/Messages/Chat streaming and buffered results, complete native replay metadata, tool-result continuation, strict completion and interruption handling, discovery limits and endpoint normalization, credential isolation, secure-store rollback, published-settings consistency, and cancellation of both pending and polling sign-in attempts. They also verify that authentication tracing does not expose codes or tokens and that applets cannot call provider administration methods. Settings regressions cover stale async responses and preserving credentials when only endpoint trailing slashes change.

Simulator smoke checks verified the provider picker, API-key versus account-sign-in forms, saving a Codex draft before choosing a model, and preserving Local demo as the active connection. An initial unsigned build exposed a Keychain access error: it lacked the simulator app identity entitlements. The normal locally signed build embeds both XML and DER entitlements and allows the connection to be saved without that error. The runnable simulator instructions now keep signing enabled.

The final app was installed and relaunched; its installed executable matched the build output. The Codex draft and active demo selection survived reinstall and relaunch, and the existing Companion applet remained installed.

These checks use fixtures and local demo mode. Live Codex/Copilot authorization, authenticated model discovery, live model generation, physical devices, and Android APK packaging were not exercised. No account credentials were entered during simulator verification. The historical checks below describe earlier implementations and their then-current limitations.

The client-owned LLM history contract was checked with:

- `pnpm test`: all 35 SDK, bridge, compiled-WASM/SQLite, JSON validation, and iOS preparation tests passed.
- `cargo test -p apell-core -p apell --offline --locked`: all 85 tests passed, including 13 loopback LLM integration tests.
- `cargo clippy --offline --locked -p apell-core -p apell --all-targets -- -D warnings`: passed.
- `pnpm tauri ios build --debug --target aarch64-sim --no-sign --ci`: the native simulator bundle, frontend, and WASM sample built successfully.
- `cargo check --offline --locked -p apell --lib --target aarch64-linux-android`: the native host and Tokn SDK passed with the installed NDK toolchain.
- `pnpm format:check` and `git diff --check`: passed.

The fixtures verify exact assistant-message metadata replay after closing a session and recreating the service, tool-result continuation without a host cache, explicit rejection of unsupported streaming metadata, buffered recovery without an automatic retry, request-scoped progress and cancellation, and authoritative partial text on failure. SDK tests cover concurrent requests, duplicate IDs, callback exceptions, abort and timeout cleanup, and rejection of the removed completion method. Compiled-WASM tests cover migration of the sample's existing SQLite schema, full message and provenance persistence across database reopen, and exclusion of interrupted or unhandled tool responses from replay.

The rebuilt host was installed and relaunched in the iOS simulator; its installed executable matched the build output. Companion was updated separately to version 0.2.0 with existing chats intact. Simulator smoke checks verified the version in About, streamed local demo replies, cancellation preserving partial text with a “Reply interrupted” label, and persistence of both completed and interrupted replies after closing and reopening the applet.

Those checks used local fixtures and demo mode, not a live provider or physical device. At that revision the host exposed the compatible Chat Completions connection through Tokn. Opaque host continuation handles and the legacy completion API had been removed.

The embedded Tokn Git integration resolves `main` to `d546ef2d306e9a952a17eae8a58366483832da05` in `Cargo.lock` and was checked with:

- `pnpm test`: all 27 SDK, bridge, compiled-WASM/SQLite, JSON validation, and iOS preparation tests passed.
- `cargo test -p apell-core -p apell --offline --locked`: all 82 tests passed, including 13 loopback LLM integration tests.
- `cargo clippy --offline --locked -p apell-core -p apell --all-targets -- -D warnings`: passed.
- `pnpm tauri ios build --debug --target aarch64-sim --no-sign --ci`: the final native iOS simulator bundle, frontend, and WASM sample built successfully.
- `cargo check --offline --locked -p apell --lib --target aarch64-linux-android`: the native host and Tokn SDK passed with the installed NDK toolchain.
- `pnpm format:check` and `git diff --check`: passed.

The new fixtures cover keyless localhost endpoints, exact URL prefixes and qualified model IDs, optional credentials, rejected invalid headers, no generation retries, redirect credential isolation, structured SSE errors, explicit completion without a `[DONE]` marker, cumulative stream bounds, and HTTP status preservation with oversized or interrupted error bodies. Existing tool/reasoning continuation and cancellation tests run through Tokn. Config tests use Tokn's authority rules; a host test verifies that an incompatible saved connection survives startup unchanged and can be corrected. A gzip fixture verifies that dependency feature unification does not silently decompress generic HTTP or download bytes.

These tests use local fixtures and do not establish live provider or physical-device behavior. This integration exposes the existing compatible Chat Completions connection; Tokn's other adapters and OAuth flows remain outside the current Apell UI.

The generic LLM contract update was checked with:

- `pnpm test`: 27 SDK, bridge, compiled-WASM/SQLite, JSON validation, and iOS preparation tests passed.
- `cargo test -p apell-core -p apell --offline`: 63 tests passed, including six real loopback HTTP integration tests for the LLM contract.
- `pnpm tauri ios build --debug --target aarch64-sim --no-sign --ci`: the WASM sample, TypeScript frontend, and native iOS simulator bundle built successfully.
- `pnpm format:check` and `git diff --check`: passed.

The LLM tests cover tool definitions and choice on the wire, multi-turn tool results, replay of older assistant reasoning through opaque handles, cross-session and modified-message rejection, connection/model changes, cancellation with authoritative partial text, truncation versus premature EOF, missing or inconsistent model provenance, sanitized provider failures, and expired/evicted continuation state. Unit tests also cover interleaved tool fragments, required/none/named choice enforcement, refusal and finish reasons, session closure, early cancellation, and legacy text-only compatibility. SDK tests keep reasoning out of partial answers and isolate duplicate and concurrent requests. The compiled sample persists non-completed outcomes as interrupted.

Provider tests use local fixtures, not a live model account. They establish the supported compatible-protocol behavior, not universal provider compatibility. Streaming `reasoning_details` remains explicitly unsupported; complete buffered metadata and text reasoning fields are covered. Physical-device testing of this extension has not been performed.

The applet controls update passed all 21 existing TypeScript tests, formatting checks, and the native iOS simulator build. Simulator smoke checks covered the top-right capsule and menu in portrait and landscape, About details, reloading with the saved conversation intact, closing to the library, and opening the shared installation sheet. Reload replaces the native session and iframe; applet bundles need no changes to receive these host controls.

The mobile launcher update was checked with:

- `pnpm test`: 21 bridge, SDK, WASM, and iOS preparation tests passed.
- `cargo test -p apell --lib --offline`: eight host and installation tests passed, including native hashing of local bundles, enforcement of supplied checksums, required checksums for URL installs, and rejection of invalid or oversized sources.
- `pnpm tauri ios build --debug --target aarch64-sim --no-sign --ci`: TypeScript, Rust WASM, production frontend, and native simulator builds passed.
- `pnpm format:check`: formatting checks passed.

The iPhone simulator reproduced the cropped desktop layout caused by explicit native window dimensions. Desktop sizing is now platform-specific, and the launcher uses one icon and label per applet with installation and settings behind header buttons. Smoke checks covered the launcher and Companion in portrait and landscape, settings, sample installation and update, opening and closing Companion, a streamed local demo reply, and conversation persistence after updating the sample. Keyboard checks covered the chat composer and revealing the focused checksum field inside the install sheet.

iOS uses a single scene and disables automatic WebView scroll insets so CSS owns safe-area padding. Before scene support was enabled, the simulator's native window retained portrait safe-area values in landscape despite reporting landscape orientation. After enabling it, both the native insets and the visible layout adjusted correctly. Temporary geometry diagnostics were removed after verification.

The HTTP, filesystem, and applet config extension was checked with:

- `pnpm test`: 12 bridge, SDK, JSON validation, and compiled-WASM integration tests passed.
- `cargo test -p apell-core -p apell --lib --offline`: 43 native-service tests and four host permission, session-lifecycle, and navigation tests passed.
- `pnpm build`: Rust WASM bundle, TypeScript checking, and production frontend build passed.
- `pnpm format:check`: formatting checks passed.
- `pnpm tauri ios build --debug --target aarch64-sim --no-sign --ci`: iOS simulator application built.
- `cargo check --offline --locked -p apell --lib --target aarch64-linux-android`: Android Rust check passed with the existing local NDK and generated project environment, including the new HTTP, file, and config services.

The new API tests exercise binary HTTP against real loopback servers, repeated headers, redirects, timeouts, cancellation and request limits; file isolation, traversal and symlink denial, atomic replacement and rename behavior; and config persistence, limits, concurrent writes, and rejection of values that JSON would silently alter. Native tests also verify manifest permissions and that session closure waits for an active local mutation and rejects subsequent work.

Xcode tool discovery has nine focused tests in `scripts/prepare-ios.test.ts`, covering stable executable paths, shell quoting, preservation of Xcode's PATH and signing settings, repeated preparation, and validation before project changes. The preparation script and tests also pass standalone TypeScript checking. With only the standard system PATH initially available, the generated local environment successfully starts Node, pnpm, Cargo, and the Tauri Xcode script. Local tool paths are stored only in the ignored generated project.

The initial implementation additionally passed one Rust chatbot domain test and produced a macOS application bundle. Its native macOS smoke test installed and opened Companion, sent a message through the Rust WASM and native LLM bridge in demo mode, observed the streamed reply, and verified that both messages survived closing and reopening the applet. The application layout was also visually inspected.

Android's aarch64 Rust library compiled, including its TLS and credential-plugin Rust branches. APK packaging did not complete: downloading Android SDK Build-Tools 36.0.0 failed with a truncated response (`Premature EOF`). Gradle was recovered and checksum-verified, but the final retry stopped before Kotlin compilation. No APK was produced. Physical-device behavior, mobile secure-store operations, and live provider requests have not yet been exercised. Demo mode is explicitly scripted; it is not evidence of a successful external model connection.

Generated applications, platform projects, local test data, and build logs are excluded from Git. Signing identities and device identifiers are not recorded here.

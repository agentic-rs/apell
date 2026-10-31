# Verification

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

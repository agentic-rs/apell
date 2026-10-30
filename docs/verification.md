# Verification

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

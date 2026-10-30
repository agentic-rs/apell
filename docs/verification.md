# Verification

The initial implementation was checked with:

- `pnpm test`: five bridge, SDK, and compiled-WASM integration tests passed.
- `cargo test -p apell-core -p apell --lib`: 22 native-service tests and one navigation-boundary test passed.
- `cargo test --manifest-path examples/chatbot/Cargo.toml`: one Rust domain test passed.
- `pnpm build`: Rust WASM bundle, TypeScript checking, and production frontend build passed.
- `pnpm format:check`: formatting checks passed.
- `pnpm tauri build --debug --bundles app --no-sign`: macOS application bundle built.
- `pnpm tauri ios build --debug --target aarch64-sim --no-sign --ci`: iOS simulator application built.

A native macOS smoke test installed and opened Companion, sent a message through the Rust WASM and native LLM bridge in demo mode, observed the streamed reply, and verified that both messages survived closing and reopening the applet. The application layout was also visually inspected.

Android's aarch64 Rust library compiled, including its TLS and credential-plugin Rust branches. APK packaging did not complete: downloading Android SDK Build-Tools 36.0.0 failed with a truncated response (`Premature EOF`). Gradle was recovered and checksum-verified, but the final retry stopped before Kotlin compilation. No APK was produced. Physical-device behavior, mobile secure-store operations, and live provider requests have not yet been exercised. Demo mode is explicitly scripted; it is not evidence of a successful external model connection.

Generated applications, platform projects, local test data, and build logs are excluded from Git. Signing identities and device identifiers are not recorded here.

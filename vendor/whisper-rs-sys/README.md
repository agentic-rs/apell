# Whisper Rust bindings

This Cargo patch uses the exact [published `whisper-rs-sys 0.15.0` archive](https://crates.io/crates/whisper-rs-sys/0.15.0).
Its SHA-256 is `6986c0fe081241d391f09b9a071fbcbb59720c3563628c3c829057cf69f2a56f`.
The Rust bindings are [Unlicense](https://unlicense.org/), and the bundled
Whisper C++ source retains its [MIT license](LICENSE.whisper.cpp).

Cargo extracts the upstream archive in its build output directory. The local
`build.rs` uses the published bindings on Android and links Apple BLAS only
when targeting Apple and the native build produced that library. The upstream
build script checked the OS of the build machine, which broke Android builds
on a Mac. Android links the NDK C++ runtime into Apell's single native library
so the APK can load it directly.

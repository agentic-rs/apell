#!/usr/bin/env bash
set -euo pipefail

source_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
install_dir="$(mktemp -d "$RUNNER_TEMP/apell-cli.XXXXXX")"
toolchain=1.99.0

if ! rustup run "$toolchain" cargo --version >/dev/null 2>&1; then
  rustup toolchain install "$toolchain" --profile minimal
fi
cd "$source_dir"
cargo "+$toolchain" install --locked --path crates/apell-cli --root "$install_dir"

printf '%s\n' "$install_dir/bin" >> "$GITHUB_PATH"
printf 'binary=%s/bin/apell\n' "$install_dir" >> "$GITHUB_OUTPUT"

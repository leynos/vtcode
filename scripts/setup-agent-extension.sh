#!/usr/bin/env bash

# Build Diapason for use with the local Zed development extension.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [[ ! -f "$REPO_ROOT/Cargo.toml" || ! -f "$REPO_ROOT/extensions/zed-extension/extension.toml" ]]; then
    printf 'Error: run this script from a Diapason repository checkout.\n' >&2
    exit 1
fi

cd "$REPO_ROOT"
printf 'Building Diapason for local Zed extension use...\n'
cargo build --release

printf '\nBuilt binary: %s\n' "$REPO_ROOT/target/release/diapason"
printf 'Install it with `cargo install --path . --locked`, or add the binary directory to PATH.\n'
printf 'Then install `extensions/zed-extension` in Zed with Install Dev Extension.\n'

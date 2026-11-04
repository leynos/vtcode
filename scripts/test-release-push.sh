#!/usr/bin/env bash

# Verify Diapason release routing without contacting GitHub or publishing.
set -euo pipefail

release_scripts=(scripts/release.sh scripts/release-lib.sh scripts/build-and-upload-binaries.sh)

for script in "${release_scripts[@]}"; do
    if grep -q 'vinhnx/homebrew-tap\|HOMEBREW_TAP_TOKEN\|x-access-token:' "$script"; then
        printf 'Unexpected external Homebrew publication remains in %s\n' "$script" >&2
        exit 1
    fi
done

grep -q 'Homebrew formula remains source-build-only' scripts/release.sh
grep -q 'Homebrew is source-build-only' scripts/build-and-upload-binaries.sh
grep -q 'gh auth switch -u leynos' scripts/release.sh
grep -q 'gh auth switch -u leynos' scripts/build-and-upload-binaries.sh

grep -q 'https://github.com/leynos/vtcode' Cargo.toml

printf 'Diapason release routing and source-only Homebrew checks passed.\n'

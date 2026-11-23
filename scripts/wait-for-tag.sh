#!/usr/bin/env bash
# Wait until a release tag is visible on the public repository.
set -euo pipefail

REPOSITORY_URL="https://github.com/leynos/vtcode.git"
GIT_BIN="${GIT_BIN:-git}"
SLEEP_BIN="${SLEEP_BIN:-sleep}"

usage() {
    printf 'Usage: %s TAG [ATTEMPTS] [DELAY_SECONDS]\n' "$(basename "$0")" >&2
}

if [ "$#" -lt 1 ] || [ "$#" -gt 3 ]; then
    usage
    exit 2
fi

tag="$1"
attempts="${2:-12}"
delay="${3:-10}"

if ! [[ "$attempts" =~ ^[1-9][0-9]*$ ]]; then
    printf 'Error: ATTEMPTS must be a positive integer, got %s\n' "$attempts" >&2
    exit 2
fi

if ! [[ "$delay" =~ ^[0-9]+$ ]]; then
    printf 'Error: DELAY_SECONDS must be a non-negative integer, got %s\n' "$delay" >&2
    exit 2
fi

for ((attempt = 1; attempt <= attempts; attempt++)); do
    if "$GIT_BIN" ls-remote --exit-code --tags "$REPOSITORY_URL" "$tag"; then
        printf 'Tag %s found\n' "$tag"
        exit 0
    fi
    printf 'Tag %s not found yet (attempt %d/%d), waiting %ss...\n' \
        "$tag" "$attempt" "$attempts" "$delay"
    "$SLEEP_BIN" "$delay"
done

printf '::error::Tag %s does not exist after %ss\n' "$tag" "$((attempts * delay))" >&2
exit 1

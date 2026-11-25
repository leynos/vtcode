#!/usr/bin/env bash
# Print a file's size in bytes, falling back to BSD stat, or "unknown".
set -euo pipefail

STAT_BIN="${STAT_BIN:-stat}"

if [ "$#" -ne 1 ]; then
    printf 'Usage: %s PATH\n' "$(basename "$0")" >&2
    exit 2
fi

path="$1"

if size="$("$STAT_BIN" -c %s "$path" 2>/dev/null)"; then
    printf '%s\n' "$size"
elif size="$("$STAT_BIN" -f %z "$path" 2>/dev/null)"; then
    printf '%s\n' "$size"
else
    printf 'unknown\n'
fi

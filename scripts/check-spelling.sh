#!/usr/bin/env bash
# Check every existing Git-tracked file against the repository spelling policy.
# Git emits NUL-delimited names, so spaces and newlines are retained exactly.
# Filtering missing paths keeps a staged deletion from turning this read-only
# gate into a failure; a failed Git discovery still stops the gate immediately.
set -euo pipefail

if [[ $# -ne 0 ]]; then
  echo "Usage: $(basename "$0")" >&2
  exit 64 # EX_USAGE
fi

GIT="${GIT:-git}"
TYPOS="${TYPOS:-typos}"

work_directory="$(mktemp -d)"
trap 'rm -rf "$work_directory"' EXIT
tracked_paths="$work_directory/tracked-paths"
existing_paths="$work_directory/existing-paths"

"$GIT" ls-files -z -- > "$tracked_paths"
while IFS= read -r -d '' tracked_path; do
  if [[ -f "$tracked_path" ]]; then
    printf '%s\0' "$tracked_path" >> "$existing_paths"
  fi
done < "$tracked_paths"
:

if [[ -s "$existing_paths" ]]; then
  if ! command -v "$TYPOS" >/dev/null 2>&1; then
    echo "$(basename "$0"): '$TYPOS' is not installed or not on PATH." >&2
    exit 127
  fi
  xargs -0 --no-run-if-empty "$TYPOS" --config typos.toml --force-exclude -- < "$existing_paths"
fi

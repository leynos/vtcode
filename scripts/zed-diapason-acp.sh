#!/bin/sh

set -eu

REPO="$(cd "$(dirname "$0")/.." && pwd)"
LOG_FILE="/tmp/zed-diapason-acp-launch.log"
STDERR_FILE="/tmp/zed-diapason-acp-stderr.log"
export RUST_LOG="${RUST_LOG:-info}"
export DIAPASON_ACP_ENABLED=1
export DIAPASON_ACP_ZED_ENABLED=1
export DIAPASON_PROVIDER=ollama

{
  echo "==== $(date '+%Y-%m-%d %H:%M:%S %z') ===="
  echo "pwd(before)=$(pwd)"
  echo "argv=$0 $*"
  echo "PATH=$PATH"
} >> "$LOG_FILE"

cd "$REPO"
exec "$REPO/target/debug/diapason" \
  --config "$REPO/config.toml" \
  --provider ollama \
  --model gpt-oss:120b-cloud \
  --api-key-env OLLAMA_API_KEY \
  --enable-skills \
  acp 2>>"$STDERR_FILE"

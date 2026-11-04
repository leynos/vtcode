#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/../.." && pwd)"

mode="fallback"
workspace="$repo_root"
host="localhost"
port="5173"
vite_pid=""

usage() {
    cat <<'EOF'
Usage: ./start.sh [mode] [options]

Modes:
  (default)       Start the browser in fallback mode.
  --headless      Start the browser and a standalone workspace bridge.
  --active        Start the browser and an interactive Diapason session.

Options:
  --workspace DIR Workspace root exposed to Diapason (default: the repository root).
  --host HOST    Browser host (default: localhost).
  --port PORT    Browser port (default: 5173).
  -h, --help     Show this help.

Usage examples:
  ./start.sh
  ./start.sh --headless --workspace /absolute/path/to/project
  ./start.sh --active --workspace /absolute/path/to/project
  ./start.sh --active --port 5174 --workspace /absolute/path/to/project
EOF
}

fail() {
    echo "Error: $*" >&2
    exit 1
}

while (($# > 0)); do
    case "$1" in
        --headless|--workspace-only)
            [[ "$mode" == "fallback" ]] || fail "choose only one start mode"
            mode="headless"
            shift
            ;;
        --active)
            [[ "$mode" == "fallback" ]] || fail "choose only one start mode"
            mode="active"
            shift
            ;;
        --workspace|-w)
            (($# >= 2)) || fail "$1 requires a directory"
            workspace="$2"
            shift 2
            ;;
        --host)
            (($# >= 2)) || fail "$1 requires a host"
            host="$2"
            shift 2
            ;;
        --port)
            (($# >= 2)) || fail "$1 requires a port"
            port="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            fail "unknown option: $1 (use --help for usage)"
            ;;
    esac
done

[[ "$port" =~ ^[0-9]+$ ]] || fail "port must be a number between 1 and 65535"
((port >= 1 && port <= 65535)) || fail "port must be a number between 1 and 65535"
[[ "$host" != *[[:space:]/]* ]] || fail "host must not contain whitespace or '/'"

[[ -d "$workspace" ]] || fail "workspace directory does not exist: $workspace"
workspace="$(cd -- "$workspace" && pwd)"
origin="http://${host}:${port}"
vite_bin="$script_dir/node_modules/.bin/vite"

command -v bun >/dev/null 2>&1 || fail "bun is required to run the Diapason WebMCP app"
if [[ ! -x "$vite_bin" ]]; then
    echo "Installing browser dependencies..."
    (cd "$script_dir" && bun install --frozen-lockfile)
fi
[[ -x "$vite_bin" ]] || fail "Vite was not installed at $vite_bin"

if [[ "$mode" != "fallback" ]]; then
    command -v cargo >/dev/null 2>&1 || fail "cargo is required for the Diapason bridge"
fi

existing_vite_pid=""
if command -v lsof >/dev/null 2>&1; then
    existing_vite_pid="$(lsof -nP -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null | head -n 1)" || true
fi

vite_is_running="false"
if [[ -n "$existing_vite_pid" ]]; then
    existing_command="$(ps -p "$existing_vite_pid" -o command= 2>/dev/null || true)"
    existing_cwd="$(lsof -a -p "$existing_vite_pid" -d cwd -Fn 2>/dev/null |
        sed -n 's/^n//p' | head -n 1)" || true
    existing_page=""
    if command -v curl >/dev/null 2>&1; then
        existing_page="$(curl -fsS --max-time 2 "$origin/" 2>/dev/null || true)"
    fi
    if [[ "$existing_command" == *"$script_dir/node_modules/.bin/vite"* ||
        "$existing_command" == *"$script_dir/node_modules/vite"* ||
        "$existing_cwd" == "$script_dir" ||
        "$existing_page" == *"<title>Diapason · WebMCP editor</title>"* ]]; then
        vite_is_running="true"
        echo "Reusing the existing WebMCP Vite server (PID $existing_vite_pid)."
    else
        fail "port $port is already used by PID $existing_vite_pid; use --port 5174 or stop that process"
    fi
fi

cleanup() {
    if [[ -n "$vite_pid" ]] && kill -0 "$vite_pid" 2>/dev/null; then
        kill "$vite_pid" 2>/dev/null || true
    fi
}

trap cleanup EXIT

start_vite() {
    if [[ "$vite_is_running" == "true" ]]; then
        return
    fi

    echo "Starting browser at $origin"
    "$vite_bin" --host "$host" --port "$port" --strictPort &
    vite_pid=$!
    sleep 1
    if ! kill -0 "$vite_pid" 2>/dev/null; then
        wait "$vite_pid" || true
        fail "Vite could not start on $origin"
    fi
}

case "$mode" in
    fallback)
        if [[ "$vite_is_running" == "true" ]]; then
            echo "Open $origin in your browser."
            exit 0
        fi

        echo "Starting browser fallback mode at $origin"
        exec "$vite_bin" --host "$host" --port "$port" --strictPort
        ;;
    headless)
        start_vite
        echo "Open $origin and pair with the URL and code printed by the bridge."
        echo "Workspace: $workspace"
        status=0
        (
            cd "$repo_root"
            cargo run --locked -- webmcp serve \
                --origin "$origin" \
                --allowed-root "$workspace"
        ) || status=$?
        exit "$status"
        ;;
    active)
        start_vite
        echo "Open $origin in your browser."
        echo "When the TUI is ready, run: /webmcp pair $origin"
        echo "Workspace: $workspace"
        status=0
        (
            cd "$repo_root"
            cargo run --locked -- --workspace "$workspace" chat
        ) || status=$?
        exit "$status"
        ;;
esac

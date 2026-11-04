#!/bin/bash

# DIAPASON - Debug Mode Launch Script
# This script provides fast development builds

set -eo pipefail

restore_terminal_state() {
	if [[ -t 1 || -t 2 ]]; then
		# Best-effort restore for raw/alternate-screen/mouse modes in case diapason aborts.
		# Order mirrors diapason_ui::tui::panic_hook::restore_tui: LeaveAlternateScreen first,
		# then bracketed paste/focus/mouse, pop keyboard flags, reset OSC 22 pointer,
		# default cursor shape, show cursor.
		printf '\r\033[K\033[?1049l\033[?2004l\033[?1004l\033[?1006l\033[?1015l\033[?1003l\033[?1002l\033[?1000l\033[<1u\033]22;default\007\033[0 q\033[?25h' >/dev/tty 2>/dev/null || true
		stty sane </dev/tty >/dev/tty 2>/dev/null || true
	fi
}

trap restore_terminal_state EXIT INT TERM

# Suppress macOS malloc warnings by REMOVING the env vars (not setting to 0)
# Setting to 0 triggers "can't turn off malloc stack logging" warnings
unset MallocStackLogging
unset MallocStackLoggingDirectory
unset MALLOCSTACKTOOLSDIR
unset MallocErrorAbort
unset MallocNanoZone

if [[ -z "${DIAPASON_SESSION_DIR:-}" ]]; then
	case "$(uname -s)" in
	Darwin)
		DIAPASON_STATE_DIR="$HOME/Library/Application Support/diapason"
		;;
	*)
		if [[ "${XDG_STATE_HOME:-}" = /* ]]; then
			DIAPASON_STATE_DIR="$XDG_STATE_HOME/diapason"
		else
			DIAPASON_STATE_DIR="$HOME/.local/state/diapason"
		fi
		;;
	esac
	export DIAPASON_SESSION_DIR="$DIAPASON_STATE_DIR/sessions"
fi

# Check if we're in the right directory
if [[ ! -f "Cargo.toml" ]]; then
	echo "Error: Please run this script from the diapason project root directory"
	exit 1
fi

# --- Fast iteration tuning -------------------------------------------------
# Keep the managed compiler wrapper enabled so this build participates in
# shared compile admission. The dev profile disables incremental compilation
# for compatibility with the shared sccache service.

# On Apple Silicon, rustc is heavy enough per-thread that pinning cargo
# jobs to the P-core count (vs. all logical CPUs) usually wins for the
# small incremental rebuilds that drive an edit->run loop: cargo's
# default counts E-cores too, and rustc threads thrashing the E-cores
# end up slower than fewer-but-faster P-core threads. No-op on non-Darwin
# hosts. Override via `CARGO_BUILD_JOBS=N ./scripts/run-debug.sh`.
if [[ -z "${CARGO_BUILD_JOBS}" && "$(uname -s)" == "Darwin" ]] &&
	sysctl -n hw.perflevel0.physicalcpu >/dev/null 2>&1; then
	export CARGO_BUILD_JOBS="$(sysctl -n hw.perflevel0.physicalcpu)"
fi

# Build optional args from environment
EXTRA_ARGS=()
if [[ -n "$MODEL" ]]; then
	EXTRA_ARGS+=(--model "$MODEL")
fi
if [[ -n "$PROVIDER" ]]; then
	EXTRA_ARGS+=(--provider "$PROVIDER")
fi
if [[ -n "$WORKSPACE" ]]; then
	EXTRA_ARGS+=(--workspace "$WORKSPACE")
fi

# Run with advanced features enabled by default.
# A single `cargo run` builds (incrementally) and launches in one pass; the
# previous `cargo build` + `cargo run` duplicated dependency-graph resolution.
# Note: Interactive chat is launched via the TUI without a subcommand.
# Increase stack floor for spawned threads in debug runs to reduce overflow risk.
export RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}"
cargo run -- "${EXTRA_ARGS[@]}" --show-file-diffs --debug

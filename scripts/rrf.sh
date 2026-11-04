#!/bin/bash

# DIAPASON - Release-Fast Run Script
# This script runs diapason with the release-fast profile for optimized performance

set -eo pipefail

# Source common utilities
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

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

# Check if we're in the right directory
if [[ ! -f "Cargo.toml" ]]; then
	echo "Error: Please run this script from the diapason project root directory"
	exit 1
fi

echo "Running diapason with release-fast profile (optimized build)..."
echo ""

# Build and run with the release-fast profile
# Increase stack floor for spawned threads to reduce overflow risk.
export RUST_MIN_STACK="${RUST_MIN_STACK:-16777216}"
cargo run --profile release-fast -- "$@"

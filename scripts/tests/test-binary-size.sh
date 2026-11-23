#!/usr/bin/env bash
# Behavioural tests for scripts/binary-size.sh with stubbed stat binaries.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SIZE_SCRIPT="$SCRIPT_DIR/../binary-size.sh"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

stub_dir="$tmp_dir/stubs"
mkdir -p "$stub_dir"
binary="$tmp_dir/diapason"
: >"$binary"

cat >"$stub_dir/stat-gnu" <<'STAT_STUB'
#!/usr/bin/env bash
# GNU stat: only -c %s is supported.
set -euo pipefail
if [ "${1:-}" = "-c" ]; then
    printf '12345\n'
    exit 0
fi
exit 1
STAT_STUB

cat >"$stub_dir/stat-bsd" <<'STAT_STUB'
#!/usr/bin/env bash
# BSD stat: -c fails, -f %z succeeds.
set -euo pipefail
if [ "${1:-}" = "-c" ]; then
    exit 1
fi
if [ "${1:-}" = "-f" ]; then
    printf '67890\n'
    exit 0
fi
exit 1
STAT_STUB

cat >"$stub_dir/stat-none" <<'STAT_STUB'
#!/usr/bin/env bash
# Neither probe is supported.
set -euo pipefail
exit 1
STAT_STUB

chmod +x "$stub_dir/stat-gnu" "$stub_dir/stat-bsd" "$stub_dir/stat-none"

failures=0
size_output=""
size_status=0

pass() {
    printf 'PASS: %s\n' "$1"
}

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    failures=$((failures + 1))
}

run_size() {
    local stat_bin="$1"
    local path="${2:-$binary}"

    set +e
    size_output="$(STAT_BIN="$stat_bin" "$SIZE_SCRIPT" "$path" 2>&1)"
    size_status=$?
    set -e
}

assert_size() {
    local expected="$1"
    local description="$2"
    if [ "$size_status" -eq 0 ] && [ "$size_output" = "$expected" ]; then
        pass "$description"
    else
        fail "$description (expected '$expected', got status $size_status output '$size_output')"
    fi
}

echo "Testing binary-size.sh GNU stat..."
run_size "$stub_dir/stat-gnu"
assert_size "12345" "GNU stat -c size is printed"

echo "Testing binary-size.sh BSD stat fallback..."
run_size "$stub_dir/stat-bsd"
assert_size "67890" "BSD stat -f size is printed when -c fails"

echo "Testing binary-size.sh unknown fallback..."
run_size "$stub_dir/stat-none"
assert_size "unknown" "unknown is printed when both probes fail"

echo "Testing binary-size.sh usage..."
if STAT_BIN="$stub_dir/stat-none" "$SIZE_SCRIPT" >"$tmp_dir/usage.out" 2>"$tmp_dir/usage.err"; then
    fail "missing PATH should exit non-zero"
else
    pass "missing PATH exits non-zero"
fi
if grep -q 'Usage:' "$tmp_dir/usage.err"; then
    pass "missing PATH prints usage"
else
    fail "missing PATH should print usage"
fi

if [ "$failures" -ne 0 ]; then
    printf 'FAILED: %d assertion(s) failed\n' "$failures" >&2
    exit 1
fi

printf 'All binary-size tests passed.\n'

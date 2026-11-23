#!/usr/bin/env bash
# Behavioural tests for scripts/wait-for-tag.sh with stubbed git and sleep.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WAIT_SCRIPT="$SCRIPT_DIR/../wait-for-tag.sh"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

stub_dir="$tmp_dir/stubs"
mkdir -p "$stub_dir"

cat >"$stub_dir/git" <<'GIT_STUB'
#!/usr/bin/env bash
# Succeed once the recorded call count reaches SUCCESS_AT.
set -euo pipefail
count=0
if [ -f "$COUNTER_FILE" ]; then
    count="$(cat "$COUNTER_FILE")"
fi
count=$((count + 1))
printf '%s\n' "$count" >"$COUNTER_FILE"
if [ "$count" -ge "$SUCCESS_AT" ]; then
    printf 'refs/tags/%s\n' "$5"
    exit 0
fi
exit 1
GIT_STUB

cat >"$stub_dir/sleep" <<'SLEEP_STUB'
#!/usr/bin/env bash
# Record each requested delay instead of sleeping.
set -euo pipefail
printf '%s\n' "$1" >>"$SLEEP_LOG"
SLEEP_STUB

chmod +x "$stub_dir/git" "$stub_dir/sleep"

failures=0
case_output=""
case_stderr=""
case_status=0
sleep_log=""

pass() {
    printf 'PASS: %s\n' "$1"
}

fail() {
    printf 'FAIL: %s\n' "$1" >&2
    failures=$((failures + 1))
}

run_case() {
    local success_at="$1"
    local tag="$2"
    local attempts="${3:-12}"
    local delay="${4:-10}"
    local case_dir
    case_dir="$(mktemp -d "$tmp_dir/case.XXXXXX")"
    sleep_log="$case_dir/sleeps"

    set +e
    COUNTER_FILE="$case_dir/counter" \
        SUCCESS_AT="$success_at" \
        SLEEP_LOG="$sleep_log" \
        GIT_BIN="$stub_dir/git" \
        SLEEP_BIN="$stub_dir/sleep" \
        "$WAIT_SCRIPT" "$tag" "$attempts" "$delay" \
        >"$case_dir/stdout" 2>"$case_dir/stderr"
    case_status=$?
    set -e

    case_output="$(cat "$case_dir/stdout" "$case_dir/stderr")"
    case_stderr="$(cat "$case_dir/stderr")"
}

assert_status() {
    local expected="$1"
    local description="$2"
    if [ "$case_status" -eq "$expected" ]; then
        pass "$description"
    else
        fail "$description (expected exit $expected, got $case_status)"
    fi
}

assert_output_contains() {
    local needle="$1"
    local description="$2"
    if [[ "$case_output" == *"$needle"* ]]; then
        pass "$description"
    else
        fail "$description (missing '$needle')"
    fi
}

assert_error_contains() {
    local needle="$1"
    local description="$2"
    if [[ "$case_stderr" == *"$needle"* ]]; then
        pass "$description"
    else
        fail "$description (stderr missing '$needle')"
    fi
}

assert_sleeps() {
    local expected_count="$1"
    local expected_delay="$2"
    local description="$3"
    local -a recorded=()
    if [ -f "$sleep_log" ]; then
        mapfile -t recorded <"$sleep_log"
    fi

    if [ "${#recorded[@]}" -ne "$expected_count" ]; then
        fail "$description (expected $expected_count sleeps, got ${#recorded[@]})"
        return
    fi

    local recorded_delay
    for recorded_delay in "${recorded[@]}"; do
        if [ "$recorded_delay" != "$expected_delay" ]; then
            fail "$description (expected ${expected_delay}s sleeps, got ${recorded_delay}s)"
            return
        fi
    done

    pass "$description"
}

echo "Testing wait-for-tag.sh first-attempt success..."
run_case 1 "v1.2.3"
assert_status 0 "first attempt exits 0"
assert_output_contains "Tag v1.2.3 found" "first attempt reports the tag"
assert_sleeps 0 10 "first attempt never sleeps"

echo "Testing wait-for-tag.sh third-attempt success..."
run_case 3 "v1.2.3"
assert_status 0 "third attempt exits 0"
assert_sleeps 2 10 "third attempt sleeps twice for 10s"
assert_output_contains "attempt 1/12" "third attempt logs attempt 1/12"
assert_output_contains "attempt 2/12" "third attempt logs attempt 2/12"
assert_output_contains "Tag v1.2.3 found" "third attempt reports the tag"

echo "Testing wait-for-tag.sh exhaustion..."
run_case 999 "v9.9.9"
assert_status 1 "exhaustion exits 1"
assert_sleeps 12 10 "exhaustion sleeps twelve times for 10s"
assert_error_contains "does not exist after 120s" "exhaustion reports the 120s deadline"
assert_output_contains "attempt 12/12" "exhaustion logs the final attempt"

echo "Testing wait-for-tag.sh custom attempts and delay..."
run_case 999 "v9.9.9" 4 3
assert_status 1 "custom exhaustion exits 1"
assert_sleeps 4 3 "custom attempts and delay are honoured"
assert_error_contains "does not exist after 12s" "custom deadline uses attempts and delay"
assert_output_contains "attempt 1/4" "custom attempts appear in diagnostics"

echo "Testing wait-for-tag.sh usage..."
if "$WAIT_SCRIPT" >"$tmp_dir/usage.out" 2>"$tmp_dir/usage.err"; then
    fail "missing TAG should exit non-zero"
else
    pass "missing TAG exits non-zero"
fi
if grep -q 'Usage:' "$tmp_dir/usage.err"; then
    pass "missing TAG prints usage"
else
    fail "missing TAG should print usage"
fi

if [ "$failures" -ne 0 ]; then
    printf 'FAILED: %d assertion(s) failed\n' "$failures" >&2
    exit 1
fi

printf 'All wait-for-tag tests passed.\n'

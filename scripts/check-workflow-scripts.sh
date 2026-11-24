#!/usr/bin/env bash
# Run the workflow process-boundary shell tests.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

passed=0
failed=0

for test_name in test-wait-for-tag.sh test-binary-size.sh; do
    test_path="$SCRIPT_DIR/tests/$test_name"
    if bash "$test_path"; then
        passed=$((passed + 1))
    else
        printf '[workflow-scripts] FAIL: %s\n' "$test_name" >&2
        failed=$((failed + 1))
    fi
done

printf '[workflow-scripts] %d passed, %d failed\n' "$passed" "$failed"

if [ "$failed" -ne 0 ]; then
    exit 1
fi

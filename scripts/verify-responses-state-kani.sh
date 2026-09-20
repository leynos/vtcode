#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
EXPECTED_VERSION=$(<"${PROJECT_ROOT}/tools/kani/VERSION")
PROOF_MANIFEST="${PROJECT_ROOT}/tools/kani/responses-reducer/Cargo.toml"

cd "${PROJECT_ROOT}"
prover-tools kani check-version --expected-version "${EXPECTED_VERSION}"

HARNESSES=(
    responses_reducer_admission_terminal_and_eof_match_independent_oracle
    responses_reducer_captures_canonical_two_call_base_state
    responses_reducer_routes_stack_identities_without_crossing_calls
    responses_reducer_separates_reasoning_sub_indexes
    responses_reducer_reconciles_snapshot_relations
    responses_reducer_final_input_preference_covers_prefix_relations
)

for harness in "${HARNESSES[@]}"; do
    # Kani enables unwind checks by default; never pass --no-unwinding-checks.
    cargo kani --manifest-path "${PROOF_MANIFEST}" --lib \
        --harness "${harness}"
done

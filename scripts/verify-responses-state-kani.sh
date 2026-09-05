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
    responses_reducer_correlation_step_preserves_canonical_two_call_state
    responses_reducer_reconciles_reasoning_snapshots_by_sub_index
    responses_reducer_reconciles_custom_input_snapshots
    responses_reducer_final_input_preference_covers_prefix_relations
)

for harness in "${HARNESSES[@]}"; do
    # Kani enables unwind checks by default; never pass --no-unwinding-checks.
    cargo kani --manifest-path "${PROOF_MANIFEST}" --lib \
        --harness "${harness}"
done

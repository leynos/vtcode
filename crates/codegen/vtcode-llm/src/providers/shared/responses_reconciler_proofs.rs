//! Bounded Kani proofs for the shared Responses stream reconciler.
//!
//! Each proof exhaustively checks one reducer invariant over a stated bound and
//! is compiled only with `--cfg kani`; unit and property tests cover the
//! unbounded and payload-projection behaviour.

use super::{
    FinalInputPreference, ResponsesItemIdentity, ResponsesStreamReconciler, ResponsesTerminalState,
    correlated_custom_track_position, reconcile_final_input, reconcile_snapshot,
};

/// Build one of the two fixed identities used by bounded proofs.
fn bounded_identity(second_call: bool) -> ResponsesItemIdentity {
    if second_call {
        ResponsesItemIdentity::new(Some("b".to_string()), Some("d".to_string()), Some(1))
    } else {
        ResponsesItemIdentity::new(Some("a".to_string()), Some("c".to_string()), Some(0))
    }
}

/// Reference admission rule: strict record highs while non-terminal.
fn oracle_admit(
    terminal: ResponsesTerminalState,
    last_sequence_rank: &mut Option<u8>,
    sequence_rank: Option<u8>,
) -> bool {
    if terminal != ResponsesTerminalState::Active {
        return false;
    }
    let Some(sequence_rank) = sequence_rank else {
        return true;
    };
    if last_sequence_rank.is_some_and(|last| sequence_rank <= last) {
        return false;
    }
    *last_sequence_rank = Some(sequence_rank);
    true
}

/// Exhaustively checks admission, terminal absorption, and EOF over traces
/// of up to six events. Sequence ranks preserve every equality and ordering
/// relation possible in a six-event trace; their absolute `u64` values are
/// irrelevant to the production comparison.
#[kani::proof]
#[kani::unwind(8)]
fn responses_reducer_admission_terminal_and_eof_match_independent_oracle() {
    let event_count = kani::any::<u8>();
    kani::assume(event_count <= 6);
    let event_kinds = kani::any::<[u8; 6]>();
    let event_has_sequence = kani::any::<[bool; 6]>();
    let event_sequence_ranks = kani::any::<[u8; 6]>();
    let mut reconciler = ResponsesStreamReconciler::default();
    let mut oracle_terminal = ResponsesTerminalState::Active;
    let mut oracle_last_sequence_rank = None;

    for index in 0..usize::from(event_count) {
        kani::assume(event_kinds[index] < 3);
        kani::assume(event_sequence_ranks[index] < 6);
        let sequence_rank = event_has_sequence[index].then_some(event_sequence_ranks[index]);
        let expected_admitted = oracle_admit(oracle_terminal, &mut oracle_last_sequence_rank, sequence_rank);
        let admitted = reconciler.admit(sequence_rank.map(u64::from));
        kani::assert(admitted == expected_admitted, "admission must match the independent oracle");
        if !admitted {
            continue;
        }

        match event_kinds[index] {
            1 => {
                reconciler.mark_completed();
                oracle_terminal = ResponsesTerminalState::Completed;
            }
            2 => {
                reconciler.mark_failed();
                oracle_terminal = ResponsesTerminalState::Failed;
            }
            _ => {}
        }
        kani::assert(
            reconciler.terminal_state() == oracle_terminal,
            "terminal transition must match the independent oracle",
        );
    }

    let terminal_before = reconciler.terminal_state();
    kani::assert(terminal_before == oracle_terminal, "the final terminal state must match the oracle");
    let admitted_after_trace = reconciler.admit(None);
    kani::assert(
        admitted_after_trace == (oracle_terminal == ResponsesTerminalState::Active),
        "unsequenced events are admitted exactly while active",
    );
    if terminal_before != ResponsesTerminalState::Active {
        match terminal_before {
            ResponsesTerminalState::Completed => reconciler.mark_failed(),
            ResponsesTerminalState::Failed => reconciler.mark_completed(),
            ResponsesTerminalState::Active => {}
        }
        kani::assert(
            reconciler.terminal_state() == terminal_before,
            "successful and failed terminal states must absorb later transitions",
        );
    }
    kani::assert(
        reconciler.require_completed().is_ok() == (terminal_before == ResponsesTerminalState::Completed),
        "EOF succeeds exactly after an admitted completed terminal",
    );
}

/// Checks that the production capture API creates two distinct empty tracks.
/// Payload projection remains covered by unit and property tests.
#[kani::proof]
#[kani::unwind(4)]
fn responses_reducer_captures_canonical_two_call_base_state() {
    let mut reconciler = ResponsesStreamReconciler::default();
    kani::assert(
        reconciler
            .capture_custom_call(bounded_identity(false), Some("e"), None, false)
            .is_ok(),
        "the first canonical call must be captured",
    );
    kani::assert(
        reconciler
            .capture_custom_call(bounded_identity(true), Some("f"), None, false)
            .is_ok(),
        "the second canonical call must be captured",
    );

    kani::assert(reconciler.terminal_state() == ResponsesTerminalState::Active, "the base state must remain active");
    kani::assert(reconciler.last_sequence_number.is_none(), "the base sequence state must be empty");
    kani::assert(reconciler.reasoning_tracks.is_empty(), "the base reasoning projection must be empty");
    kani::assert(
        reconciler.custom_call_tracks.len() == 2,
        "the base representation must contain exactly two custom tracks",
    );
    kani::assert(
        reconciler.custom_call_tracks[0].identity.item_id.as_deref() == Some("a")
            && reconciler.custom_call_tracks[0].identity.call_id.as_deref() == Some("c")
            && reconciler.custom_call_tracks[0].identity.output_index == Some(0)
            && reconciler.custom_call_tracks[0].identity.sub_index.is_none(),
        "the first base representation identity must be canonical",
    );
    kani::assert(
        reconciler.custom_call_tracks[1].identity.item_id.as_deref() == Some("b")
            && reconciler.custom_call_tracks[1].identity.call_id.as_deref() == Some("d")
            && reconciler.custom_call_tracks[1].identity.output_index == Some(1)
            && reconciler.custom_call_tracks[1].identity.sub_index.is_none(),
        "the second base representation identity must be canonical",
    );
    kani::assert(
        reconciler.custom_call_tracks[0].name.as_deref() == Some("e")
            && reconciler.custom_call_tracks[0].input.is_empty(),
        "the first base track payload must be exact",
    );
    kani::assert(
        reconciler.custom_call_tracks[1].name.as_deref() == Some("f")
            && reconciler.custom_call_tracks[1].input.is_empty(),
        "the second base track payload must be exact",
    );
    // The proof covers reducer state, not allocator teardown. Avoid
    // expanding destructor paths after all postconditions are checked.
    std::mem::forget(reconciler);
}

/// Build a reasoning identity from output and sub-part indexes.
fn reasoning_identity(output_index: usize, sub_index: usize) -> ResponsesItemIdentity {
    ResponsesItemIdentity::new(None, None, Some(output_index)).with_sub_index(Some(sub_index))
}
/// Exhaustively classifies immutable lookup against two registered calls.
/// Registration history, payload append, and projection remain unit/property
/// test responsibilities.
/// The bound also covers bytewise comparison of the full error diagnostics.
#[kani::proof]
#[kani::unwind(64)]
fn responses_reducer_routes_stack_identities_without_crossing_calls() {
    let registered = [bounded_identity(false), bounded_identity(true)];
    let query_kind = kani::any::<u8>();
    kani::assume(query_kind < 8);
    let query = match query_kind {
        0 => bounded_identity(false),
        1 => bounded_identity(true),
        2 => ResponsesItemIdentity::new(None, Some("c".to_string()), None),
        3 => ResponsesItemIdentity::new(Some("b".to_string()), None, None),
        4 => ResponsesItemIdentity::new(None, None, Some(2)),
        5 => ResponsesItemIdentity::new(Some("a".to_string()), None, Some(1)),
        6 => ResponsesItemIdentity::default(),
        _ => ResponsesItemIdentity::default().with_sub_index(Some(0)),
    };
    let actual = correlated_custom_track_position(registered.iter(), &query);
    match query_kind {
        0 | 2 => kani::assert(actual == Ok(Some(0)), "the first call must be selected"),
        1 | 3 => kani::assert(actual == Ok(Some(1)), "the second call must be selected"),
        4 | 7 => kani::assert(actual == Ok(None), "an unknown keyed identity must not match"),
        5 => kani::assert(
            actual == Err("custom tool call correlation aliases conflict"),
            "conflict must dominate a partial match",
        ),
        _ => kani::assert(
            actual == Err("custom tool input event has no correlation identity"),
            "an empty identity must be rejected",
        ),
    }
    std::mem::forget(query);
    std::mem::forget(registered);
}

/// Proves index-only reasoning identities match exactly when both output
/// and part indexes match.
#[kani::proof]
fn responses_reducer_separates_reasoning_sub_indexes() {
    let left_output = kani::any::<bool>();
    let right_output = kani::any::<bool>();
    let left_part = kani::any::<bool>();
    let right_part = kani::any::<bool>();
    let left_identity = reasoning_identity(usize::from(left_output), usize::from(left_part));
    let right_identity = reasoning_identity(usize::from(right_output), usize::from(right_part));
    kani::assert(
        left_identity.reasoning_matches(&right_identity) == (left_output == right_output && left_part == right_part),
        "both reasoning indexes must exactly determine index-only correlation",
    );
}

/// Covers all four done-snapshot relations directly on the production
/// reconciler helper. Multi-track correlation remains ordinary-tested.
#[kani::proof]
#[kani::unwind(4)]
fn responses_reducer_reconciles_snapshot_relations() {
    let snapshot_case = kani::any::<u8>();
    kani::assume(snapshot_case < 4);
    let mut accumulated = "a".to_string();
    let snapshot = match snapshot_case {
        0 => "ax",
        1 => "a",
        2 => "",
        _ => "z",
    };
    let actual = reconcile_snapshot(&mut accumulated, snapshot);
    match snapshot_case {
        0 => kani::assert(
            actual == Ok(Some("x".to_string())) && accumulated == "ax",
            "an extension must append and return only its suffix",
        ),
        1 | 2 => kani::assert(
            actual == Ok(None) && accumulated == "a",
            "equal and stale snapshots must preserve accumulated input",
        ),
        _ => kani::assert(actual.is_err() && accumulated == "a", "divergence must not rewrite input"),
    }
    std::mem::forget(actual);
    std::mem::forget(accumulated);
}

/// Covers every branch of the final-versus-streamed prefix policy using a
/// representative of each relation class.
#[kani::proof]
#[kani::unwind(4)]
fn responses_reducer_final_input_preference_covers_prefix_relations() {
    let relation_case = kani::any::<u8>();
    kani::assume(relation_case < 4);
    let (streamed, final_input) = match relation_case {
        0 => ("", ""),
        1 => ("a", "ab"),
        2 => ("ab", "a"),
        _ => ("a", "b"),
    };
    let result = reconcile_final_input(streamed, final_input);
    match relation_case {
        0 | 1 => {
            kani::assert(result == Ok(FinalInputPreference::Final), "equal or extending final input must be preferred")
        }
        2 => kani::assert(
            result == Ok(FinalInputPreference::Streamed),
            "a stale final input must preserve the longer streamed value",
        ),
        _ => kani::assert(result.is_err(), "divergent final and streamed inputs must be rejected"),
    }
}

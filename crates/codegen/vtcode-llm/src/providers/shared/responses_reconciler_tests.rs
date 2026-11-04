use super::{
    FinalInputPreference, ReasoningChannel, ResponsesItemIdentity, ResponsesStreamReconciler, ResponsesTerminalState,
    reconcile_final_input,
};
use proptest::prelude::*;

/// Build an identity for one indexed item and call.
fn item(index: usize) -> ResponsesItemIdentity {
    ResponsesItemIdentity::new(Some(format!("item_{index}")), Some(format!("call_{index}")), Some(index))
}

struct ReasoningReconciliationCase<'a> {
    channel: ReasoningChannel,
    delta: &'a str,
    snapshot: &'a str,
    expected_suffix: &'a str,
}

/// Assert that one reasoning channel emits its delta and only the unseen snapshot suffix.
fn assert_reasoning_channel_reconciles(
    reconciler: &mut ResponsesStreamReconciler,
    identity: &ResponsesItemIdentity,
    case: ReasoningReconciliationCase<'_>,
) {
    let identity = identity.clone().with_reasoning_channel(case.channel);
    assert_eq!(reconciler.reasoning_delta(identity.clone(), case.delta), Ok(case.delta.to_string()));
    assert_eq!(reconciler.reasoning_done(identity, case.snapshot), Ok(Some(case.expected_suffix.to_string())));
}

/// Prove repeated deltas survive while their done snapshot is not replayed.
#[test]
fn repeated_reasoning_deltas_are_preserved_but_done_snapshot_is_not_replayed() {
    let mut reconciler = ResponsesStreamReconciler::default();
    let identity = item(0);

    assert!(reconciler.admit(Some(1)));
    assert_eq!(reconciler.reasoning_delta(identity.clone(), "ha"), Ok("ha".to_string()));
    assert!(reconciler.admit(Some(2)));
    assert_eq!(reconciler.reasoning_delta(identity.clone(), "ha"), Ok("ha".to_string()));
    assert!(reconciler.admit(Some(3)));
    assert_eq!(reconciler.reasoning_done(identity, "haha"), Ok(None));
}

/// Prove a completed terminal absorbs later failed transitions and admissions.
#[test]
fn terminal_state_is_absorbing() {
    let mut reconciler = ResponsesStreamReconciler::default();
    assert!(reconciler.admit(Some(1)));
    reconciler.mark_completed();
    reconciler.mark_failed();

    assert_eq!(reconciler.terminal_state(), ResponsesTerminalState::Completed);
    assert!(!reconciler.admit(Some(2)));
    assert!(!reconciler.admit(None));
    assert_eq!(reconciler.require_completed(), Ok(()));
}

/// Prove EOF without a completed event is not a success terminal.
#[test]
fn eof_is_not_a_success_terminal() {
    let reconciler = ResponsesStreamReconciler::default();
    assert_eq!(reconciler.require_completed(), Err("stream ended without a response.completed event"));
}

/// Prove interleaved custom calls keep their raw input correlated per call.
#[test]
fn two_interleaved_custom_calls_keep_raw_input_correlated() {
    let mut reconciler = ResponsesStreamReconciler::default();
    reconciler.capture_custom_call(item(0), Some("patch"), None).expect("metadata");
    reconciler.capture_custom_call(item(1), Some("shell"), None).expect("metadata");
    let _ = reconciler.custom_input_delta(item(0), "*** Begin").expect("first delta");
    let _ = reconciler.custom_input_delta(item(1), "cargo ").expect("second delta");
    let _ = reconciler.custom_input_delta(item(0), " Patch").expect("first delta");
    let _ = reconciler.custom_input_done(item(1), "cargo check").expect("second done");

    let calls = reconciler.custom_tool_calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].input, "*** Begin Patch");
    assert_eq!(calls[1].input, "cargo check");
}

/// Prove contradictory custom-call aliases are rejected without mutating input.
#[test]
fn contradictory_custom_call_aliases_are_rejected() {
    let mut reconciler = ResponsesStreamReconciler::default();
    reconciler.capture_custom_call(item(0), Some("patch"), None).expect("metadata");
    let contradictory =
        ResponsesItemIdentity::new(Some("item_0".to_string()), Some("call_changed".to_string()), Some(0));

    assert_eq!(
        reconciler.custom_input_delta(contradictory, "unsafe"),
        Err("custom tool call correlation aliases conflict")
    );
    assert_eq!(reconciler.custom_tool_calls()[0].input, "");
}

/// Prove reasoning sub-indexes reconcile independently within one item.
#[test]
fn reasoning_sub_indexes_reconcile_independently() {
    let mut reconciler = ResponsesStreamReconciler::default();
    let first = ResponsesItemIdentity::new(Some("reasoning_1".to_string()), None, Some(0)).with_sub_index(Some(0));
    let second = ResponsesItemIdentity::new(Some("reasoning_1".to_string()), None, Some(0)).with_sub_index(Some(1));

    assert_eq!(reconciler.reasoning_delta(first.clone(), "first"), Ok("first".to_string()));
    assert_eq!(reconciler.reasoning_delta(second.clone(), "second"), Ok("second".to_string()));
    assert_eq!(reconciler.reasoning_done(first, "first"), Ok(None));
    assert_eq!(reconciler.reasoning_done(second, "second+"), Ok(Some("+".to_string())));
}

/// Prove raw and summary channels reconcile independently over shared indexes.
#[test]
fn reasoning_channels_reconcile_independently_with_shared_indexes() {
    let mut reconciler = ResponsesStreamReconciler::default();
    let identity = item(0).with_sub_index(Some(0));

    assert_reasoning_channel_reconciles(
        &mut reconciler,
        &identity,
        ReasoningReconciliationCase {
            channel: ReasoningChannel::Raw,
            delta: "raw",
            snapshot: "raw+",
            expected_suffix: "+",
        },
    );
    assert_reasoning_channel_reconciles(
        &mut reconciler,
        &identity,
        ReasoningReconciliationCase {
            channel: ReasoningChannel::Summary,
            delta: "summary",
            snapshot: "summary+",
            expected_suffix: "+",
        },
    );
}

/// Prove channel-only reasoning frames share one track.
#[test]
fn channel_only_reasoning_frames_share_one_track() {
    let mut reconciler = ResponsesStreamReconciler::default();
    let identity = ResponsesItemIdentity::default().with_reasoning_channel(ReasoningChannel::Summary);

    assert_eq!(reconciler.reasoning_delta(identity.clone(), "first"), Ok("first".to_string()));
    assert_eq!(reconciler.reasoning_delta(identity.clone(), "second"), Ok("second".to_string()));
    assert_eq!(reconciler.reasoning_done(identity, "firstsecond"), Ok(None));
}

/// Prove completed tool input must extend, or be extended by, streamed input.
#[test]
fn final_tool_input_must_extend_or_be_extended_by_streamed_input() {
    assert_eq!(reconcile_final_input("abc", "abcdef"), Ok(FinalInputPreference::Final));
    assert_eq!(reconcile_final_input("abcdef", "abc"), Ok(FinalInputPreference::Streamed));
    assert_eq!(reconcile_final_input("abc", "abX"), Err("completed tool input diverges from streamed prefix"));
}

proptest! {
    /// Prove a done snapshot emits only the suffix not already emitted.
    #[test]
    fn reasoning_done_emits_only_the_unseen_suffix(
        deltas in prop::collection::vec("[a-z]{0,8}", 0..12),
        suffix in "[a-z]{0,8}",
    ) {
        let mut reconciler = ResponsesStreamReconciler::default();
        let identity = item(0);
        let expected = deltas.concat();
        let mut emitted = String::new();
        for delta in &deltas {
            emitted.push_str(&reconciler.reasoning_delta(identity.clone(), delta).expect("reasoning track"));
        }
        prop_assert_eq!(&emitted, &expected);
        let snapshot = format!("{expected}{suffix}");
        let done_delta = reconciler.reasoning_done(identity, &snapshot);

        let actual_done_delta = match done_delta {
            Ok(delta) => delta,
            Err(message) => return Err(TestCaseError::fail(message)),
        };
        if let Some(actual_done_delta) = actual_done_delta {
            emitted.push_str(&actual_done_delta);
        }
        prop_assert_eq!(emitted, snapshot);
    }

    /// Prove admitted sequence numbers are strict record highs.
    #[test]
    fn admitted_sequence_numbers_are_strict_record_highs(sequence_numbers in prop::collection::vec(any::<u16>(), 0..64)) {
        let mut reconciler = ResponsesStreamReconciler::default();
        let expected = sequence_numbers
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                sequence_numbers[..index].iter().all(|prior| prior < candidate).then_some(*candidate)
            })
            .collect::<Vec<_>>();
        let mut admitted = Vec::new();
        // Exercise an equal-to-high-water repeat in every non-empty case,
        // rather than relying on rare collisions in uniform u16 samples.
        for sequence_number in sequence_numbers.iter().copied().chain(sequence_numbers.iter().max().copied()) {
            if reconciler.admit(Some(u64::from(sequence_number))) {
                admitted.push(sequence_number);
            }
        }
        prop_assert_eq!(admitted, expected);
    }

    /// Prove interleaved custom delta histories preserve per-call bytes.
    #[test]
    fn interleaved_custom_delta_histories_preserve_per_call_bytes(
        history in prop::collection::vec((any::<bool>(), "[ -~]{0,8}"), 0..32),
    ) {
        let mut reconciler = ResponsesStreamReconciler::default();
        prop_assert!(reconciler.capture_custom_call(item(0), Some("first"), None).is_ok());
        prop_assert!(reconciler.capture_custom_call(item(1), Some("second"), None).is_ok());
        let mut expected = [String::new(), String::new()];

        for (second_call, delta) in history {
            let index = usize::from(second_call);
            expected[index].push_str(&delta);
            let actual = reconciler.custom_input_delta(item(index), &delta);
            prop_assert_eq!(actual.as_deref(), Ok(delta.as_str()));
        }

        let calls = reconciler.custom_tool_calls();
        prop_assert_eq!(calls.len(), 2);
        prop_assert_eq!(&calls[0].input, &expected[0]);
        prop_assert_eq!(&calls[1].input, &expected[1]);
    }

    /// Prove arbitrary reasoning chunk partitions reassemble exact bytes.
    #[test]
    fn arbitrary_reasoning_chunk_partitions_reassemble_exact_bytes(
        chunks in prop::collection::vec("[ -~]{0,12}", 0..24),
    ) {
        let expected = chunks.concat();
        let mut reconciler = ResponsesStreamReconciler::default();
        let identity = item(0).with_sub_index(Some(0));
        let mut actual = String::new();
        for chunk in chunks {
            actual.push_str(&reconciler.reasoning_delta(identity.clone(), &chunk).expect("reasoning track"));
        }
        prop_assert_eq!(&actual, &expected);
        prop_assert_eq!(reconciler.reasoning_done(identity, &expected), Ok(None));
    }

    /// Prove conflicting alias mutations do not cross-route generated input.
    #[test]
    fn conflicting_alias_mutations_do_not_cross_route_generated_input(delta in ".{0,32}") {
        let mut reconciler = ResponsesStreamReconciler::default();
        prop_assert!(reconciler.capture_custom_call(item(0), Some("tool"), None).is_ok());
        prop_assert!(reconciler.custom_input_delta(item(0), &delta).is_ok());
        let contradictory = ResponsesItemIdentity::new(
            Some("item_0".to_string()),
            Some("call_changed".to_string()),
            Some(0),
        );

        prop_assert!(reconciler.custom_input_delta(contradictory, "mutated").is_err());
        let calls = reconciler.custom_tool_calls();
        prop_assert_eq!(calls.len(), 1);
        prop_assert_eq!(&calls[0].input, &delta);
    }

    /// Prove divergent done mutations are rejected without changing the prefix.
    #[test]
    fn divergent_done_mutations_are_rejected_without_changing_generated_prefix(prefix in ".{0,32}") {
        let streamed = format!("a{prefix}");
        let divergent = format!("b{prefix}");
        let mut reconciler = ResponsesStreamReconciler::default();
        let identity = item(0).with_sub_index(Some(0));
        let actual = reconciler.reasoning_delta(identity.clone(), &streamed).expect("reasoning track");
        prop_assert_eq!(&actual, &streamed);

        prop_assert!(reconciler.reasoning_done(identity.clone(), &divergent).is_err());
        prop_assert_eq!(reconciler.reasoning_done(identity.clone(), &streamed), Ok(None));
        let extended = format!("{streamed}x");
        prop_assert_eq!(reconciler.reasoning_done(identity, &extended), Ok(Some("x".to_string())));
    }

    /// Prove terminal states absorb all follow-up events.
    #[test]
    fn terminal_mutations_absorb_all_generated_followup_events(
        completed in any::<bool>(),
        followups in prop::collection::vec((any::<u64>(), ".{0,16}"), 0..24),
    ) {
        let mut reconciler = ResponsesStreamReconciler::default();
        if completed {
            reconciler.mark_completed();
        } else {
            reconciler.mark_failed();
        }
        let terminal = reconciler.terminal_state();

        for (sequence_number, delta) in followups {
            let admitted = reconciler.admit(Some(sequence_number));
            prop_assert!(!admitted);
            if admitted {
                let _ = reconciler.reasoning_delta(item(0), &delta).expect("reasoning track");
            }
        }
        prop_assert_eq!(reconciler.terminal_state(), terminal);
    }
}

//! Deterministic state reconciliation for Responses API streams.
//!
//! This module deliberately has no async or wire-format dependencies. Both
//! Responses stream decoders drive the same production reducer, and the Kani
//! harness below exhaustively exercises that reducer with bounded traces.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ResponsesTerminalState {
    #[default]
    Active,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ResponsesItemIdentity {
    pub(crate) item_id: Option<String>,
    pub(crate) call_id: Option<String>,
    pub(crate) output_index: Option<usize>,
    pub(crate) sub_index: Option<usize>,
}

impl ResponsesItemIdentity {
    pub(crate) fn new(item_id: Option<String>, call_id: Option<String>, output_index: Option<usize>) -> Self {
        Self { item_id, call_id, output_index, sub_index: None }
    }

    pub(crate) fn with_sub_index(mut self, sub_index: Option<usize>) -> Self {
        self.sub_index = sub_index;
        self
    }

    fn has_key(&self) -> bool {
        self.item_id.as_deref().is_some_and(|value| !value.is_empty())
            || self.call_id.as_deref().is_some_and(|value| !value.is_empty())
            || self.output_index.is_some()
            || self.sub_index.is_some()
    }

    fn reasoning_matches(&self, other: &Self) -> bool {
        if let (Some(left), Some(right)) = (self.sub_index, other.sub_index) {
            return left == right
                && optional_strings_compatible(self.item_id.as_deref(), other.item_id.as_deref())
                && optional_values_compatible(self.output_index, other.output_index);
        }

        optional_strings_equal(self.item_id.as_deref(), other.item_id.as_deref())
            || matches!((self.output_index, other.output_index), (Some(left), Some(right)) if left == right)
    }

    fn call_relation(&self, other: &Self) -> IdentityRelation {
        let comparisons = [
            compare_optional(nonempty(self.call_id.as_deref()), nonempty(other.call_id.as_deref())),
            compare_optional(nonempty(self.item_id.as_deref()), nonempty(other.item_id.as_deref())),
            compare_optional(self.output_index, other.output_index),
        ];
        let has_equal = comparisons.contains(&Some(true));
        let has_different = comparisons.contains(&Some(false));
        match (has_equal, has_different) {
            (true, true) => IdentityRelation::Conflict,
            (true, false) => IdentityRelation::Match,
            _ => IdentityRelation::Distinct,
        }
    }

    fn merge_from(&mut self, other: &Self) {
        if self.item_id.as_deref().is_none_or(str::is_empty) {
            self.item_id.clone_from(&other.item_id);
        }
        if self.call_id.as_deref().is_none_or(str::is_empty) {
            self.call_id.clone_from(&other.call_id);
        }
        if self.output_index.is_none() {
            self.output_index = other.output_index;
        }
        if self.sub_index.is_none() {
            self.sub_index = other.sub_index;
        }
    }

    pub(crate) fn response_call_id(&self) -> Option<&str> {
        nonempty(self.call_id.as_deref()).or_else(|| nonempty(self.item_id.as_deref()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IdentityRelation {
    Match,
    Distinct,
    Conflict,
}

fn compare_optional<T: PartialEq>(left: Option<T>, right: Option<T>) -> Option<bool> {
    Some(left? == right?)
}

fn optional_strings_equal(left: Option<&str>, right: Option<&str>) -> bool {
    matches!((nonempty(left), nonempty(right)), (Some(left), Some(right)) if left == right)
}

fn optional_strings_compatible(left: Option<&str>, right: Option<&str>) -> bool {
    match (nonempty(left), nonempty(right)) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

fn optional_values_compatible<T: PartialEq>(left: Option<T>, right: Option<T>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

fn correlated_custom_track_position<'a, I>(
    mut identities: I,
    identity: &ResponsesItemIdentity,
) -> Result<Option<usize>, &'static str>
where
    I: Clone + Iterator<Item = &'a ResponsesItemIdentity>,
{
    if !identity.has_key() {
        return Err("custom tool input event has no correlation identity");
    }
    if identities
        .clone()
        .any(|registered| registered.call_relation(identity) == IdentityRelation::Conflict)
    {
        return Err("custom tool call correlation aliases conflict");
    }
    Ok(identities.position(|registered| registered.call_relation(identity) == IdentityRelation::Match))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TextTrack {
    identity: ResponsesItemIdentity,
    text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CustomCallTrack {
    identity: ResponsesItemIdentity,
    name: Option<String>,
    input: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReconciledCustomToolCall {
    pub(crate) call_id: String,
    pub(crate) name: String,
    pub(crate) input: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FinalInputPreference {
    Final,
    Streamed,
}

pub(crate) fn reconcile_final_input(streamed: &str, final_input: &str) -> Result<FinalInputPreference, &'static str> {
    if final_input.starts_with(streamed) {
        Ok(FinalInputPreference::Final)
    } else if streamed.starts_with(final_input) {
        Ok(FinalInputPreference::Streamed)
    } else {
        Err("completed tool input diverges from streamed prefix")
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ResponsesStreamReconciler {
    last_sequence_number: Option<u64>,
    terminal: ResponsesTerminalState,
    reasoning_tracks: Vec<TextTrack>,
    custom_call_tracks: Vec<CustomCallTrack>,
}

impl ResponsesStreamReconciler {
    /// Admit one wire event according to the Responses sequence contract.
    ///
    /// Unsequenced compatibility events are accepted while active. Sequenced
    /// events must be strictly newer than the last accepted sequenced event;
    /// duplicate and stale frames are ignored. Once terminal, all later frames
    /// are ignored, making both successful and failed terminal states absorbing.
    pub(crate) fn admit(&mut self, sequence_number: Option<u64>) -> bool {
        if self.terminal != ResponsesTerminalState::Active {
            return false;
        }

        let Some(sequence_number) = sequence_number else {
            return true;
        };
        if self.last_sequence_number.is_some_and(|last| sequence_number <= last) {
            return false;
        }

        self.last_sequence_number = Some(sequence_number);
        true
    }

    pub(crate) fn mark_completed(&mut self) {
        if self.terminal == ResponsesTerminalState::Active {
            self.terminal = ResponsesTerminalState::Completed;
        }
    }

    pub(crate) fn mark_failed(&mut self) {
        if self.terminal == ResponsesTerminalState::Active {
            self.terminal = ResponsesTerminalState::Failed;
        }
    }

    pub(crate) fn terminal_state(&self) -> ResponsesTerminalState {
        self.terminal
    }

    /// Validate the stream at EOF. Only the explicit successful terminal is
    /// accepted; transport EOF and failed/incomplete terminals cannot commit.
    pub(crate) fn require_completed(&self) -> Result<(), &'static str> {
        match self.terminal {
            ResponsesTerminalState::Completed => Ok(()),
            ResponsesTerminalState::Active => Err("stream ended without a response.completed event"),
            ResponsesTerminalState::Failed => Err("stream ended in a failed terminal state"),
        }
    }

    /// Record a real delta. Text equality is intentionally irrelevant: equal
    /// deltas at different sequence numbers are legitimate and both survive.
    pub(crate) fn reasoning_delta(&mut self, identity: ResponsesItemIdentity, delta: &str) -> String {
        let track = find_or_insert_text_track(&mut self.reasoning_tracks, identity);
        track.text.push_str(delta);
        delta.to_string()
    }

    /// Reconcile a `*.done` snapshot with deltas already emitted for the item.
    /// The snapshot may add a missing suffix, but it cannot rewrite an emitted
    /// prefix. A shorter snapshot is treated as stale and emits nothing.
    pub(crate) fn reasoning_done(
        &mut self,
        identity: ResponsesItemIdentity,
        snapshot: &str,
    ) -> Result<Option<String>, &'static str> {
        let track = find_or_insert_text_track(&mut self.reasoning_tracks, identity);
        reconcile_snapshot(&mut track.text, snapshot)
    }

    pub(crate) fn capture_custom_call(
        &mut self,
        identity: ResponsesItemIdentity,
        name: Option<&str>,
        input_snapshot: Option<&str>,
    ) -> Result<(), &'static str> {
        let track = self.custom_track(identity)?;
        if track.name.as_deref().is_none_or(str::is_empty)
            && let Some(name) = nonempty(name)
        {
            track.name = Some(name.to_string());
        }
        if let Some(input_snapshot) = input_snapshot {
            reconcile_snapshot(&mut track.input, input_snapshot)?;
        }
        Ok(())
    }

    pub(crate) fn custom_input_delta(
        &mut self,
        identity: ResponsesItemIdentity,
        delta: &str,
    ) -> Result<String, &'static str> {
        self.custom_track(identity)?.input.push_str(delta);
        Ok(delta.to_string())
    }

    pub(crate) fn custom_input_done(
        &mut self,
        identity: ResponsesItemIdentity,
        snapshot: &str,
    ) -> Result<Option<String>, &'static str> {
        let track = self.custom_track(identity)?;
        reconcile_snapshot(&mut track.input, snapshot)
    }

    pub(crate) fn custom_tool_calls(&self) -> Vec<ReconciledCustomToolCall> {
        self.custom_call_tracks
            .iter()
            .filter_map(|track| {
                Some(ReconciledCustomToolCall {
                    call_id: track.identity.response_call_id()?.to_string(),
                    name: nonempty(track.name.as_deref())?.to_string(),
                    input: track.input.clone(),
                })
            })
            .collect()
    }

    fn custom_track(&mut self, identity: ResponsesItemIdentity) -> Result<&mut CustomCallTrack, &'static str> {
        let position =
            correlated_custom_track_position(self.custom_call_tracks.iter().map(|track| &track.identity), &identity)?;
        if let Some(position) = position {
            let track = &mut self.custom_call_tracks[position];
            track.identity.merge_from(&identity);
            return Ok(track);
        }

        self.custom_call_tracks
            .push(CustomCallTrack { identity, name: None, input: String::new() });
        Ok(self
            .custom_call_tracks
            .last_mut()
            .expect("a custom call track was just inserted"))
    }
}

fn find_or_insert_text_track(tracks: &mut Vec<TextTrack>, identity: ResponsesItemIdentity) -> &mut TextTrack {
    if let Some(position) = tracks.iter().position(|track| {
        track.identity.reasoning_matches(&identity) || (!track.identity.has_key() && !identity.has_key())
    }) {
        let track = &mut tracks[position];
        track.identity.merge_from(&identity);
        return track;
    }

    tracks.push(TextTrack { identity, text: String::new() });
    tracks.last_mut().expect("a text track was just inserted")
}

fn reconcile_snapshot(accumulated: &mut String, snapshot: &str) -> Result<Option<String>, &'static str> {
    if snapshot.starts_with(accumulated.as_str()) {
        let suffix = snapshot[accumulated.len()..].to_string();
        accumulated.push_str(&suffix);
        return Ok((!suffix.is_empty()).then_some(suffix));
    }
    if accumulated.starts_with(snapshot) {
        return Ok(None);
    }

    Err("done snapshot diverges from streamed prefix")
}

#[cfg(test)]
mod tests {
    use super::{
        FinalInputPreference, ResponsesItemIdentity, ResponsesStreamReconciler, ResponsesTerminalState,
        reconcile_final_input,
    };
    use proptest::prelude::*;

    fn item(index: usize) -> ResponsesItemIdentity {
        ResponsesItemIdentity::new(Some(format!("item_{index}")), Some(format!("call_{index}")), Some(index))
    }

    #[test]
    fn repeated_reasoning_deltas_are_preserved_but_done_snapshot_is_not_replayed() {
        let mut reconciler = ResponsesStreamReconciler::default();
        let identity = item(0);

        assert!(reconciler.admit(Some(1)));
        assert_eq!(reconciler.reasoning_delta(identity.clone(), "ha"), "ha");
        assert!(reconciler.admit(Some(2)));
        assert_eq!(reconciler.reasoning_delta(identity.clone(), "ha"), "ha");
        assert!(reconciler.admit(Some(3)));
        assert_eq!(reconciler.reasoning_done(identity, "haha"), Ok(None));
    }

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

    #[test]
    fn eof_is_not_a_success_terminal() {
        let reconciler = ResponsesStreamReconciler::default();
        assert_eq!(reconciler.require_completed(), Err("stream ended without a response.completed event"));
    }

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

    #[test]
    fn reasoning_sub_indexes_reconcile_independently() {
        let mut reconciler = ResponsesStreamReconciler::default();
        let first = ResponsesItemIdentity::new(Some("reasoning_1".to_string()), None, Some(0)).with_sub_index(Some(0));
        let second = ResponsesItemIdentity::new(Some("reasoning_1".to_string()), None, Some(0)).with_sub_index(Some(1));

        assert_eq!(reconciler.reasoning_delta(first.clone(), "first"), "first");
        assert_eq!(reconciler.reasoning_delta(second.clone(), "second"), "second");
        assert_eq!(reconciler.reasoning_done(first, "first"), Ok(None));
        assert_eq!(reconciler.reasoning_done(second, "second+"), Ok(Some("+".to_string())));
    }

    #[test]
    fn final_tool_input_must_extend_or_be_extended_by_streamed_input() {
        assert_eq!(reconcile_final_input("abc", "abcdef"), Ok(FinalInputPreference::Final));
        assert_eq!(reconcile_final_input("abcdef", "abc"), Ok(FinalInputPreference::Streamed));
        assert_eq!(reconcile_final_input("abc", "abX"), Err("completed tool input diverges from streamed prefix"));
    }

    proptest! {
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
                emitted.push_str(&reconciler.reasoning_delta(identity.clone(), delta));
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

        #[test]
        fn arbitrary_reasoning_chunk_partitions_reassemble_exact_bytes(
            chunks in prop::collection::vec("[ -~]{0,12}", 0..24),
        ) {
            let expected = chunks.concat();
            let mut reconciler = ResponsesStreamReconciler::default();
            let identity = item(0).with_sub_index(Some(0));
            let mut actual = String::new();
            for chunk in chunks {
                actual.push_str(&reconciler.reasoning_delta(identity.clone(), &chunk));
            }
            prop_assert_eq!(&actual, &expected);
            prop_assert_eq!(reconciler.reasoning_done(identity, &expected), Ok(None));
        }

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

        #[test]
        fn divergent_done_mutations_are_rejected_without_changing_generated_prefix(prefix in ".{0,32}") {
            let streamed = format!("a{prefix}");
            let divergent = format!("b{prefix}");
            let mut reconciler = ResponsesStreamReconciler::default();
            let identity = item(0).with_sub_index(Some(0));
            let actual = reconciler.reasoning_delta(identity.clone(), &streamed);
            prop_assert_eq!(&actual, &streamed);

            prop_assert!(reconciler.reasoning_done(identity.clone(), &divergent).is_err());
            prop_assert_eq!(reconciler.reasoning_done(identity.clone(), &streamed), Ok(None));
            let extended = format!("{streamed}x");
            prop_assert_eq!(reconciler.reasoning_done(identity, &extended), Ok(Some("x".to_string())));
        }

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
                    let _ = reconciler.reasoning_delta(item(0), &delta);
                }
            }
            prop_assert_eq!(reconciler.terminal_state(), terminal);
        }
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::{
        FinalInputPreference, ResponsesItemIdentity, ResponsesStreamReconciler, ResponsesTerminalState,
        correlated_custom_track_position, reconcile_final_input, reconcile_snapshot,
    };

    fn bounded_identity(second_call: bool) -> ResponsesItemIdentity {
        if second_call {
            ResponsesItemIdentity::new(Some("b".to_string()), Some("d".to_string()), Some(1))
        } else {
            ResponsesItemIdentity::new(Some("a".to_string()), Some("c".to_string()), Some(0))
        }
    }

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
            reconciler.capture_custom_call(bounded_identity(false), Some("e"), None).is_ok(),
            "the first canonical call must be captured",
        );
        kani::assert(
            reconciler.capture_custom_call(bounded_identity(true), Some("f"), None).is_ok(),
            "the second canonical call must be captured",
        );

        kani::assert(
            reconciler.terminal_state() == ResponsesTerminalState::Active,
            "the base state must remain active",
        );
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
            left_identity.reasoning_matches(&right_identity)
                == (left_output == right_output && left_part == right_part),
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
            0 | 1 => kani::assert(
                result == Ok(FinalInputPreference::Final),
                "equal or extending final input must be preferred",
            ),
            2 => kani::assert(
                result == Ok(FinalInputPreference::Streamed),
                "a stale final input must preserve the longer streamed value",
            ),
            _ => kani::assert(result.is_err(), "divergent final and streamed inputs must be rejected"),
        }
    }
}

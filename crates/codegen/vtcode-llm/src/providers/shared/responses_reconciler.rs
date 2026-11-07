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
    reasoning_channel: Option<ReasoningChannel>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReasoningChannel {
    Raw,
    Summary,
}

impl ResponsesItemIdentity {
    /// Build an identity from the provider-declared item, call, and output positions.
    pub(crate) fn new(item_id: Option<String>, call_id: Option<String>, output_index: Option<usize>) -> Self {
        Self {
            item_id,
            call_id,
            output_index,
            sub_index: None,
            reasoning_channel: None,
        }
    }

    /// Attach the sub-part index used by reasoning summary and content parts.
    pub(crate) fn with_sub_index(mut self, sub_index: Option<usize>) -> Self {
        self.sub_index = sub_index;
        self
    }

    /// Attach the reasoning channel that keeps raw and summary tracks apart.
    pub(crate) fn with_reasoning_channel(mut self, reasoning_channel: ReasoningChannel) -> Self {
        self.reasoning_channel = Some(reasoning_channel);
        self
    }

    /// Return whether any identity field is populated.
    fn has_key(&self) -> bool {
        self.item_id.as_deref().is_some_and(|value| !value.is_empty())
            || self.call_id.as_deref().is_some_and(|value| !value.is_empty())
            || self.output_index.is_some()
            || self.sub_index.is_some()
            || self.reasoning_channel.is_some()
    }

    /// Decide whether two identities address the same reasoning track.
    fn reasoning_matches(&self, other: &Self) -> bool {
        if self.reasoning_channel != other.reasoning_channel {
            return false;
        }

        if let (Some(left), Some(right)) = (self.sub_index, other.sub_index) {
            return left == right
                && optional_strings_compatible(self.item_id.as_deref(), other.item_id.as_deref())
                && optional_values_compatible(self.output_index, other.output_index);
        }

        if !self.has_reasoning_correlation_key() && !other.has_reasoning_correlation_key() {
            // Only the channel identifies these frames; keep one track per channel.
            return true;
        }

        optional_strings_equal(self.item_id.as_deref(), other.item_id.as_deref())
            || matches!((self.output_index, other.output_index), (Some(left), Some(right)) if left == right)
    }

    /// Return whether this identity can correlate a reasoning part.
    fn has_reasoning_correlation_key(&self) -> bool {
        self.item_id.as_deref().is_some_and(|value| !value.is_empty())
            || self.output_index.is_some()
            || self.sub_index.is_some()
    }

    /// Classify two call identities as a match, distinct, or conflicting aliases.
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

    /// Fill missing identity fields from a later registration.
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

    /// Return the call or item identifier used by the completed response.
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

/// Compare two optional values, treating a missing side as unknown.
fn compare_optional<T: PartialEq>(left: Option<T>, right: Option<T>) -> Option<bool> {
    Some(left? == right?)
}

/// Compare non-empty strings, treating blank values as missing.
fn optional_strings_equal(left: Option<&str>, right: Option<&str>) -> bool {
    matches!((nonempty(left), nonempty(right)), (Some(left), Some(right)) if left == right)
}

/// Compare non-empty strings without rejecting an unknown side.
fn optional_strings_compatible(left: Option<&str>, right: Option<&str>) -> bool {
    match (nonempty(left), nonempty(right)) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

/// Compare optional values without rejecting an unknown side.
fn optional_values_compatible<T: PartialEq>(left: Option<T>, right: Option<T>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

/// Drop an empty or missing string.
fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// Return the position of the identity matching `identity`, rejecting ambiguity.
///
/// A bridging event can match more than one registered track when each track
/// holds a different alias. Choosing one arbitrarily would merge unrelated
/// calls, so an ambiguous match is rejected before any state changes.
fn correlated_custom_track_position<'a, I>(
    identities: I,
    identity: &ResponsesItemIdentity,
) -> Result<Option<usize>, &'static str>
where
    I: Clone + Iterator<Item = &'a ResponsesItemIdentity>,
{
    if !identity.has_key() {
        return Err("custom tool input event has no correlation identity");
    }
    let mut matched: Option<usize> = None;
    for (position, registered) in identities.enumerate() {
        match registered.call_relation(identity) {
            IdentityRelation::Conflict => return Err("custom tool call correlation aliases conflict"),
            IdentityRelation::Match if matched.is_some() => {
                return Err("custom tool call correlation matches multiple tracks");
            }
            IdentityRelation::Match => matched = Some(position),
            IdentityRelation::Distinct => {}
        }
    }
    Ok(matched)
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
    /// Whether a completed input snapshot (`*.done`) has been observed.
    input_complete: bool,
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

/// Choose the longer consistent input between streamed and completed payloads.
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

    /// Mark the stream completed; later terminal transitions are ignored.
    pub(crate) fn mark_completed(&mut self) {
        if self.terminal == ResponsesTerminalState::Active {
            self.terminal = ResponsesTerminalState::Completed;
        }
    }

    /// Mark the stream failed; later terminal transitions are ignored.
    pub(crate) fn mark_failed(&mut self) {
        if self.terminal == ResponsesTerminalState::Active {
            self.terminal = ResponsesTerminalState::Failed;
        }
    }

    /// Return the current absorbing terminal state.
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
    pub(crate) fn reasoning_delta(
        &mut self,
        identity: ResponsesItemIdentity,
        delta: &str,
    ) -> Result<String, &'static str> {
        let track = find_or_insert_text_track(&mut self.reasoning_tracks, identity)?;
        track.text.push_str(delta);
        Ok(delta.to_string())
    }

    /// Reconcile a `*.done` snapshot with deltas already emitted for the item.
    /// The snapshot may add a missing suffix, but it cannot rewrite an emitted
    /// prefix. A shorter snapshot is treated as stale and emits nothing.
    pub(crate) fn reasoning_done(
        &mut self,
        identity: ResponsesItemIdentity,
        snapshot: &str,
    ) -> Result<Option<String>, &'static str> {
        let track = find_or_insert_text_track(&mut self.reasoning_tracks, identity)?;
        reconcile_snapshot(&mut track.text, snapshot)
    }

    /// Capture custom-call metadata without replaying an emitted prefix.
    ///
    /// `input_complete` states whether the source frame was a completed item
    /// snapshot (`output_item.done`), not merely an added one. Only completed
    /// input may later be adopted as an executable call.
    pub(crate) fn capture_custom_call(
        &mut self,
        identity: ResponsesItemIdentity,
        name: Option<&str>,
        input_snapshot: Option<&str>,
        input_complete: bool,
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
        track.input_complete |= input_complete;
        Ok(())
    }

    /// Append one streamed custom-tool input delta and return it.
    pub(crate) fn custom_input_delta(
        &mut self,
        identity: ResponsesItemIdentity,
        delta: &str,
    ) -> Result<String, &'static str> {
        self.custom_track(identity)?.input.push_str(delta);
        Ok(delta.to_string())
    }

    /// Reconcile a completed custom-tool input snapshot with streamed deltas.
    ///
    /// A `*.done` snapshot proves the provider finished the input, so the
    /// track becomes eligible for recovery from the streamed aggregate.
    pub(crate) fn custom_input_done(
        &mut self,
        identity: ResponsesItemIdentity,
        snapshot: &str,
    ) -> Result<Option<String>, &'static str> {
        let track = self.custom_track(identity)?;
        let delta = reconcile_snapshot(&mut track.input, snapshot)?;
        track.input_complete = true;
        Ok(delta)
    }

    /// Return every announced custom call: a name and a correlating identity.
    ///
    /// Announced calls are used to detect a completed response that contradicts
    /// the stream. They are not necessarily executable: `input_complete` may
    /// still be `false`.
    pub(crate) fn announced_custom_tool_calls(&self) -> Vec<ReconciledCustomToolCall> {
        self.custom_call_tracks.iter().filter_map(reconciled_custom_call).collect()
    }

    /// Return adoptable custom calls: completed input, a name, and an identity.
    ///
    /// Calls whose input never reached a completed snapshot stay out of the
    /// aggregate so partial streamed input cannot become an executable call.
    pub(crate) fn completed_custom_tool_calls(&self) -> Vec<ReconciledCustomToolCall> {
        self.custom_call_tracks
            .iter()
            .filter(|track| track.input_complete)
            .filter_map(reconciled_custom_call)
            .collect()
    }

    /// Find or create the custom-call track for an identity, rejecting alias conflicts.
    fn custom_track(&mut self, identity: ResponsesItemIdentity) -> Result<&mut CustomCallTrack, &'static str> {
        let position =
            correlated_custom_track_position(self.custom_call_tracks.iter().map(|track| &track.identity), &identity)?;
        if let Some(position) = position {
            let track = &mut self.custom_call_tracks[position];
            track.identity.merge_from(&identity);
            return Ok(track);
        }

        self.custom_call_tracks.push(CustomCallTrack {
            identity,
            name: None,
            input: String::new(),
            input_complete: false,
        });
        self.custom_call_tracks
            .last_mut()
            .ok_or("custom call track insertion did not retain the new track")
    }
}

/// Project one custom-call track into its wire-facing form.
fn reconciled_custom_call(track: &CustomCallTrack) -> Option<ReconciledCustomToolCall> {
    Some(ReconciledCustomToolCall {
        call_id: track.identity.response_call_id()?.to_string(),
        name: nonempty(track.name.as_deref())?.to_string(),
        input: track.input.clone(),
    })
}

/// Find or create a reasoning text track, keeping channels and sub-indexes apart.
fn find_or_insert_text_track(
    tracks: &mut Vec<TextTrack>,
    identity: ResponsesItemIdentity,
) -> Result<&mut TextTrack, &'static str> {
    if let Some(position) = tracks.iter().position(|track| {
        track.identity.reasoning_matches(&identity) || (!track.identity.has_key() && !identity.has_key())
    }) {
        let track = &mut tracks[position];
        track.identity.merge_from(&identity);
        return Ok(track);
    }

    tracks.push(TextTrack { identity, text: String::new() });
    tracks.last_mut().ok_or("text track insertion did not retain the new track")
}

/// Extend an accumulated snapshot, rejecting a divergent rewrite.
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
#[path = "responses_reconciler_tests.rs"]
mod tests;

#[cfg(kani)]
#[path = "responses_reconciler_proofs.rs"]
mod kani_proofs;

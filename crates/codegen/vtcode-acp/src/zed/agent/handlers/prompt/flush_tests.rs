//! Unit coverage for the buffered-text flush decision used when a provider
//! failure races `session/cancel`.
//!
//! These cases are not reachable deterministically over the duplex transport:
//! cancellation normally wins the select and drops the provider future, so the
//! failure-after-cancel branch only covers a narrow race window.

use super::*;

use proptest::prelude::*;

/// Builds output for a draft that has or has not been shown to the client.
fn output(message: &str, assistant_emitted: bool) -> ProviderOutput {
    ProviderOutput {
        assistant_message: message.to_string(),
        assistant_reasoning: String::new(),
        emitted_output: !message.is_empty(),
        assistant_emitted,
    }
}

#[test]
/// An unstreamed buffered draft becomes visible exactly once.
fn unemitted_draft_is_flushed_exactly_once() {
    let mut draft = output("partial draft", false);

    assert_eq!(draft.take_unemitted_assistant_text().as_deref(), Some("partial draft"));
    assert!(draft.assistant_emitted, "flushing must mark the draft as client-visible");
    assert_eq!(draft.take_unemitted_assistant_text(), None, "the draft must not be sent twice");
}

#[test]
/// A draft the client already streamed is not sent again on cancel.
fn streamed_draft_is_not_resent_on_cancel() {
    let mut streamed = output("streamed draft", true);

    assert_eq!(streamed.take_unemitted_assistant_text(), None);
    assert_eq!(streamed.assistant_message, "streamed draft");
}

#[test]
/// Cancelling with no draft emits no assistant chunk.
fn empty_draft_sends_nothing() {
    let mut empty = output("", false);

    assert_eq!(empty.take_unemitted_assistant_text(), None);
    assert!(!empty.assistant_emitted);
}

#[test]
/// A flushed draft stays marked visible across a tool-round reset.
fn flushed_visibility_survives_a_tool_round_reset() {
    let mut draft = output("partial draft", false);

    drop(draft.take_unemitted_assistant_text());
    draft.reset();

    assert!(draft.assistant_emitted, "a flushed chunk stays visible across a tool round");
    assert!(draft.assistant_message.is_empty());
}

proptest! {
    /// Bounded model of a turn's drafts: however many drafts and tool-round
    /// resets occur, no draft that was not already visible is lost, and none is
    /// published twice.
    #[test]
    fn each_unseen_draft_is_published_exactly_once(
        drafts in proptest::collection::vec("[a-z ]{0,16}", 1..8),
        initially_emitted in any::<bool>(),
    ) {
        let mut visible = initially_emitted;
        let mut draft = output("", visible);
        for text in drafts {
            draft.assistant_message = text.clone();
            draft.emitted_output = !text.is_empty();

            let first = draft.take_unemitted_assistant_text();
            let second = draft.take_unemitted_assistant_text();

            if text.is_empty() || visible {
                prop_assert_eq!(first, None, "visible or empty text must not be published");
            } else {
                prop_assert_eq!(first.as_deref(), Some(text.as_str()));
                visible = true;
            }
            prop_assert_eq!(second, None, "a draft must never be published twice");

            draft.reset();
            draft.assistant_emitted = visible;
        }
    }
}

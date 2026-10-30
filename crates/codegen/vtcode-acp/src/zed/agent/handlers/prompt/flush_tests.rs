//! Unit coverage for the buffered-text flush decision used when a provider
//! failure races `session/cancel`.
//!
//! These cases are not reachable deterministically over the duplex transport:
//! cancellation normally wins the select and drops the provider future, so the
//! failure-after-cancel branch only covers a narrow race window.

use super::*;

fn output(message: &str, assistant_emitted: bool) -> ProviderOutput {
    ProviderOutput {
        assistant_message: message.to_string(),
        assistant_reasoning: String::new(),
        emitted_output: !message.is_empty(),
        assistant_emitted,
    }
}

#[test]
fn unemitted_draft_is_flushed_exactly_once() {
    let mut draft = output("partial draft", false);

    assert_eq!(draft.take_unemitted_assistant_text().as_deref(), Some("partial draft"));
    assert!(draft.assistant_emitted, "flushing must mark the draft as client-visible");
    assert_eq!(draft.take_unemitted_assistant_text(), None, "the draft must not be sent twice");
}

#[test]
fn streamed_draft_is_not_resent_on_cancel() {
    let mut streamed = output("streamed draft", true);

    assert_eq!(streamed.take_unemitted_assistant_text(), None);
    assert_eq!(streamed.assistant_message, "streamed draft");
}

#[test]
fn empty_draft_sends_nothing() {
    let mut empty = output("", false);

    assert_eq!(empty.take_unemitted_assistant_text(), None);
    assert!(!empty.assistant_emitted);
}

#[test]
fn flushed_visibility_survives_a_tool_round_reset() {
    let mut draft = output("partial draft", false);

    drop(draft.take_unemitted_assistant_text());
    draft.reset();

    assert!(draft.assistant_emitted, "a flushed chunk stays visible across a tool round");
    assert!(draft.assistant_message.is_empty());
}

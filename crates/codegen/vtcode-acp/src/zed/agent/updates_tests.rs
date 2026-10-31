//! Verifies ordering and emptiness handling for ACP assistant updates.
use super::updates::{AssistantChunkKind, assistant_chunk_kind, assistant_updates};
use crate::acp;
use rstest::rstest;

fn text_of(update: &acp::SessionUpdate) -> &str {
    let chunk = match update {
        acp::SessionUpdate::AgentThoughtChunk(chunk) | acp::SessionUpdate::AgentMessageChunk(chunk) => chunk,
        _ => panic!("expected an assistant chunk"),
    };
    match &chunk.content {
        acp::ContentBlock::Text(text) => &text.text,
        _ => panic!("expected text content"),
    }
}

#[rstest]
#[case::reasoning_before_commentary(
    "Checking files.",
    "I need the workspace listing.",
    &[
        (AssistantChunkKind::Reasoning, "I need the workspace listing."),
        (AssistantChunkKind::Commentary, "Checking files."),
    ],
)]
#[case::commentary_only(
    "Checking files.",
    "",
    &[(AssistantChunkKind::Commentary, "Checking files.")],
)]
#[case::reasoning_only(
    "",
    "I need the workspace listing.",
    &[(AssistantChunkKind::Reasoning, "I need the workspace listing.")],
)]
fn assistant_updates_emit_each_non_empty_channel_in_order(
    #[case] content: &str,
    #[case] reasoning: &str,
    #[case] expected: &[(AssistantChunkKind, &str)],
) {
    let updates = assistant_updates(content, reasoning);

    assert_eq!(updates.len(), expected.len());
    for (update, (kind, text)) in updates.iter().zip(expected) {
        assert_eq!(assistant_chunk_kind(update), Some(*kind));
        assert_eq!(text_of(update), *text);
    }
}

#[test]
fn empty_content_and_reasoning_emit_nothing() {
    assert!(assistant_updates("", "").is_empty());
}

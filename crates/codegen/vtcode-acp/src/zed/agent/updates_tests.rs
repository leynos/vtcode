//! Verifies ordering and emptiness handling for ACP assistant updates.
use super::updates::{AssistantChunkKind, assistant_chunk_kind, assistant_updates};
use crate::acp;

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

#[test]
fn reasoning_and_content_emit_thought_before_message() {
    let updates = assistant_updates("Checking files.", "I need the workspace listing.");

    assert_eq!(updates.len(), 2);
    assert!(matches!(&updates[0], acp::SessionUpdate::AgentThoughtChunk(_)));
    assert_eq!(assistant_chunk_kind(&updates[0]), Some(AssistantChunkKind::Reasoning));
    assert_eq!(text_of(&updates[0]), "I need the workspace listing.");
    assert!(matches!(&updates[1], acp::SessionUpdate::AgentMessageChunk(_)));
    assert_eq!(assistant_chunk_kind(&updates[1]), Some(AssistantChunkKind::Commentary));
    assert_eq!(text_of(&updates[1]), "Checking files.");
}

#[test]
fn content_only_emits_a_message() {
    let updates = assistant_updates("Checking files.", "");

    assert_eq!(updates.len(), 1);
    assert!(matches!(&updates[0], acp::SessionUpdate::AgentMessageChunk(_)));
    assert_eq!(assistant_chunk_kind(&updates[0]), Some(AssistantChunkKind::Commentary));
    assert_eq!(text_of(&updates[0]), "Checking files.");
}

#[test]
fn reasoning_only_emits_a_thought() {
    let updates = assistant_updates("", "I need the workspace listing.");

    assert_eq!(updates.len(), 1);
    assert!(matches!(&updates[0], acp::SessionUpdate::AgentThoughtChunk(_)));
    assert_eq!(assistant_chunk_kind(&updates[0]), Some(AssistantChunkKind::Reasoning));
    assert_eq!(text_of(&updates[0]), "I need the workspace listing.");
}

#[test]
fn empty_content_and_reasoning_emit_nothing() {
    assert!(assistant_updates("", "").is_empty());
}

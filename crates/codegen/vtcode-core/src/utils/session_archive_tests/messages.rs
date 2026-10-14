//! Verifies conversion and serialization of archived message content, reasoning, tool calls, and metadata.
use super::*;

#[test]
fn session_message_roundtrip_preserves_content_and_reasoning() {
    let mut original = Message::assistant("Test response".to_owned());
    original.reasoning = Some("Model thoughts".to_owned());
    let stored = SessionMessage::from(&original);
    let restored = Message::from(&stored);

    assert_eq!(original.role, restored.role);
    assert_eq!(original.content, restored.content);
    assert_eq!(original.reasoning, restored.reasoning);
    assert_eq!(original.tool_call_id, restored.tool_call_id);
}

#[test]
fn session_message_roundtrip_preserves_incomplete_response_metadata() {
    let mut original = Message::assistant("Partial response".to_owned());
    original.metadata = Some(crate::core::message_metadata::MessageMetadata::incomplete_llm_response(
        2_000,
        12,
        "stream disconnected",
    ));
    let stored = SessionMessage::from(&original);
    let restored = Message::from(&stored);

    assert!(
        stored
            .metadata
            .as_deref()
            .is_some_and(crate::core::message_metadata::MessageMetadata::is_incomplete)
    );
    assert!(
        restored
            .metadata
            .as_ref()
            .is_some_and(crate::core::message_metadata::MessageMetadata::is_incomplete)
    );
}

#[test]
fn session_message_roundtrip_preserves_final_answer_phase() {
    let original = Message::assistant("Final response".to_owned()).with_phase(Some(AssistantPhase::FinalAnswer));
    let stored = SessionMessage::from(&original);
    let restored = Message::from(&stored);

    assert_eq!(stored.phase, Some(AssistantPhase::FinalAnswer));
    assert_eq!(restored.phase, Some(AssistantPhase::FinalAnswer));
}

#[test]
fn session_message_roundtrip_preserves_commentary_phase() {
    let original = Message::assistant("Working".to_owned()).with_phase(Some(AssistantPhase::Commentary));
    let stored = SessionMessage::from(&original);
    let restored = Message::from(&stored);

    assert_eq!(stored.phase, Some(AssistantPhase::Commentary));
    assert_eq!(restored.phase, Some(AssistantPhase::Commentary));
}

#[test]
fn session_message_roundtrip_preserves_turn_scoped_clear_scope() {
    let original = Message::turn_scoped_system("Only you see the output".to_owned());
    let stored = SessionMessage::from(&original);
    let encoded = serde_json::to_vec(&stored).expect("session message serialization");
    let decoded: SessionMessage = serde_json::from_slice(&encoded).expect("session message deserialization");
    let restored = Message::from(&decoded);

    assert_eq!(decoded.clear_at, Some(MessageClearAt::NextUserMessage));
    assert_eq!(restored.clear_at, Some(MessageClearAt::NextUserMessage));
}

#[test]
fn session_message_preserves_reasoning_details() {
    let mut original = Message::assistant("Calling a tool".to_owned());
    original.reasoning_details = Some(vec![serde_json::json!({
        "summary": "tool call planning"
    })]);

    let stored = SessionMessage::from(&original);
    let restored = Message::from(&stored);

    assert_eq!(stored.reasoning_details.as_deref(), original.reasoning_details.as_ref());
    assert_eq!(restored.reasoning_details, original.reasoning_details);
}

#[test]
fn session_message_preserves_tool_calls() {
    let mut original = Message::assistant("Calling a tool".to_owned());
    original.tool_calls = Some(vec![ToolCall::function(
        "call_1".to_string(),
        "command_session".to_string(),
        "{\"cmd\":\"cargo fmt\"}".to_string(),
    )]);

    let stored = SessionMessage::from(&original);
    let restored = Message::from(&stored);

    assert_eq!(stored.tool_calls.as_deref(), original.tool_calls.as_ref());
    assert_eq!(restored.tool_calls, original.tool_calls);
}

#[test]
fn session_message_preserves_origin_tool() {
    let mut original = Message::assistant("Calling a tool".to_owned());
    original.origin_tool = Some("command_session".to_string());

    let stored = SessionMessage::from(&original);
    let restored = Message::from(&stored);

    assert_eq!(stored.origin_tool.as_deref().map(|v| v.as_str()), original.origin_tool.as_deref());
    assert_eq!(restored.origin_tool, original.origin_tool);
}

#[test]
fn session_message_discards_empty_string_metadata_during_conversion() {
    let mut original = Message::assistant("Calling a tool".to_owned());
    original.reasoning = Some(String::new());
    original.tool_call_id = Some(String::new());
    original.origin_tool = Some(String::new());

    let stored = SessionMessage::from(&original);
    assert!(stored.reasoning.is_none());
    assert!(stored.tool_call_id.is_none());
    assert!(stored.origin_tool.is_none());

    let restored = Message::from(&stored);
    assert!(restored.reasoning.is_none());
    assert!(restored.tool_call_id.is_none());
    assert!(restored.origin_tool.is_none());
}

#[test]
fn session_message_discards_empty_collection_metadata_during_conversion() {
    let mut original = Message::assistant("Calling a tool".to_owned());
    original.reasoning_details = Some(Vec::new());
    original.tool_calls = Some(Vec::new());

    let stored = SessionMessage::from(&original);
    assert!(stored.reasoning_details.is_none());
    assert!(stored.tool_calls.is_none());

    let restored = Message::from(&stored);
    assert!(restored.reasoning_details.is_none());
    assert!(restored.tool_calls.is_none());
}

#[test]
fn session_message_deserialize_discards_empty_string_metadata() {
    let message = deserialize_session_message(serde_json::json!({
        "role": "Assistant",
        "content": "done",
        "reasoning": "",
        "tool_call_id": "",
        "origin_tool": "",
    }));

    assert!(message.reasoning.is_none());
    assert!(message.tool_call_id.is_none());
    assert!(message.origin_tool.is_none());
}

#[test]
fn session_message_deserialize_discards_empty_collection_metadata() {
    let message = deserialize_session_message(serde_json::json!({
        "role": "Assistant",
        "content": "done",
        "reasoning_details": [],
        "tool_calls": [],
    }));

    assert!(message.reasoning_details.is_none());
    assert!(message.tool_calls.is_none());
}

fn deserialize_session_message(payload: serde_json::Value) -> SessionMessage {
    serde_json::from_value(payload).expect("session message")
}

#[test]
fn session_message_preserves_parts() {
    let original = Message::assistant_with_parts(vec![
        ContentPart::text("See attached image".to_owned()),
        ContentPart::text("See attached image".to_owned()),
        ContentPart::image("encoded-image".to_owned(), "image/png".to_owned()),
        ContentPart::image("encoded-image".to_owned(), "image/png".to_owned()),
        ContentPart::image("encoded-image".to_owned(), "image/png".to_owned()),
    ]);
    let stored = SessionMessage::from(&original);

    assert_eq!(stored.content, original.content);

    let restored = Message::from(&stored);
    assert_eq!(restored.content, original.content);
}

#[test]
fn session_message_layout_is_smaller_than_unboxed_equivalent() {
    #[derive(Debug)]
    #[expect(
        dead_code,
        reason = "Intentional compatibility, platform, test, or API-shape suppression."
    )]
    struct UnboxedSessionMessageLike(
        MessageRole,
        MessageContent,
        Option<String>,
        Option<Vec<serde_json::Value>>,
        Option<Vec<ToolCall>>,
        Option<String>,
        Option<AssistantPhase>,
        Option<String>,
    );

    assert!(size_of::<SessionMessage>() < size_of::<UnboxedSessionMessageLike>());
}

#[test]
fn boxed_session_progress_option_is_pointer_sized() {
    assert_eq!(size_of::<Option<Box<SessionProgress>>>(), size_of::<usize>());
    assert!(size_of::<Option<SessionProgress>>() > size_of::<Option<Box<SessionProgress>>>());
}

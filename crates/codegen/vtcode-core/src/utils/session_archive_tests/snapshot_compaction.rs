use super::*;

#[test]
fn snapshot_compaction_shrinks_large_single_message_payloads() -> Result<()> {
    let snapshot = SessionSnapshot {
        metadata: SessionArchiveMetadata::new("Workspace", "/tmp/workspace", "model", "provider", "dark", "medium"),
        started_at: Utc::now(),
        ended_at: Utc::now(),
        total_messages: 1,
        distinct_tools: vec!["very-large-tool-name".repeat(20)],
        transcript: vec!["transcript ".repeat(400)],
        messages: vec![SessionMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Parts(vec![
                ContentPart::text("text ".repeat(800)),
                ContentPart::image("a".repeat(4000), "image/png".to_string()),
            ]),
            reasoning: Some(Box::new("reasoning ".repeat(300))),
            reasoning_details: None,
            tool_calls: Some(Box::new(vec![ToolCall::function(
                "call_1".to_string(),
                "command_session".to_string(),
                "{\"cmd\":\"echo giant payload\"}".repeat(100),
            )])),
            tool_call_id: None,
            phase: None,
            origin_tool: Some(Box::new("command_session".repeat(50))),
            metadata: None,
            clear_at: None,
        }],
        progress: Some(Box::new(SessionProgress {
            turn_number: 1,
            recent_messages: vec![SessionMessage::new(MessageRole::Assistant, "recent ".repeat(500))],
            tool_summaries: vec!["summary ".repeat(100)],
            token_usage: Some("token ".repeat(200)),
            max_context_tokens: Some(128_000),
            loaded_skills: vec!["skill ".repeat(50)],
            turn_diagnostics: None,
        })),
        error_logs: vec![ErrorLogEntry {
            timestamp: Utc::now().to_rfc3339(),
            level: "ERROR".to_string(),
            target: "vtcode_test".to_string(),
            message: "error ".repeat(400),
        }],
    };

    let compacted = compact_snapshot_to_max_bytes(snapshot, 2_048)?;

    assert!(serde_json::to_vec(&compacted)?.len() <= 2_048);
    assert_eq!(compacted.messages.len(), 1);
    Ok(())
}

#[test]
fn client_metadata_is_bounded_by_entry_count_and_serialized_size() {
    let mut entries = serde_json::Map::new();
    for index in 0..MAX_CLIENT_METADATA_ENTRIES + 4 {
        entries.insert(format!("key-{index:02}"), serde_json::json!("value"));
    }
    let mut metadata = Some(entries);

    assert!(bound_client_metadata(&mut metadata));
    let bounded = metadata.expect("nonempty client metadata should be retained");
    assert_eq!(bounded.len(), MAX_CLIENT_METADATA_ENTRIES);
    assert!(bounded.contains_key("key-00"));
    assert!(!bounded.contains_key(&format!("key-{:02}", MAX_CLIENT_METADATA_ENTRIES)));
    assert!(serialized_json_size_at_most(&bounded, MAX_CLIENT_METADATA_BYTES).is_some());

    let mut entries = serde_json::Map::new();
    entries.insert("k".to_owned(), serde_json::Value::String("x".repeat(MAX_CLIENT_METADATA_BYTES - 8)));
    let mut metadata = Some(entries);

    assert!(!bound_client_metadata(&mut metadata));
    let bounded = metadata.expect("metadata at the byte limit should be retained");
    assert_eq!(serialized_json_size_at_most(&bounded, MAX_CLIENT_METADATA_BYTES), Some(MAX_CLIENT_METADATA_BYTES));

    let mut entries = serde_json::Map::new();
    entries.insert("a-oversized".to_owned(), serde_json::Value::String("x".repeat(MAX_CLIENT_METADATA_BYTES * 2)));
    entries.insert("z-small".to_owned(), serde_json::json!("retained"));
    let mut metadata = Some(entries);

    assert!(bound_client_metadata(&mut metadata));
    let bounded = metadata.expect("metadata after dropping an oversized value should remain");
    assert_eq!(bounded.len(), 1);
    assert!(bounded.contains_key("z-small"));
    assert!(serialized_json_size_at_most(&bounded, MAX_CLIENT_METADATA_BYTES).is_some());
}

#[test]
fn snapshot_compaction_reduces_client_metadata_to_fit_the_total_size_limit() -> Result<()> {
    let mut metadata =
        SessionArchiveMetadata::new("Workspace", "/tmp/workspace", "model", "provider", "dark", "medium");
    metadata.client_metadata = Some(
        (0..MAX_CLIENT_METADATA_ENTRIES)
            .map(|index| (format!("key-{index:02}"), serde_json::json!("value".repeat(40))))
            .collect(),
    );
    let snapshot = SessionSnapshot {
        metadata,
        started_at: Utc::now(),
        ended_at: Utc::now(),
        total_messages: 0,
        distinct_tools: Vec::new(),
        transcript: Vec::new(),
        messages: Vec::new(),
        progress: None,
        error_logs: Vec::new(),
    };

    let compacted = compact_snapshot_to_max_bytes(snapshot, 1_024)?;
    assert!(serde_json::to_vec(&compacted)?.len() <= 1_024);
    assert!(
        compacted
            .metadata
            .client_metadata
            .is_none_or(|metadata| metadata.len() < MAX_CLIENT_METADATA_ENTRIES)
    );
    Ok(())
}

//! Checks snapshot size reduction and bounded persistence of namespaced extensions.
use super::*;
use proptest::prelude::*;
use std::collections::BTreeMap;

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
fn archive_extensions_are_bounded_by_namespace_count_and_serialized_size() {
    let mut extensions = SessionArchiveExtensions::new();
    for index in 0..MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES + 4 {
        drop(extensions.insert(format!("org.example.namespace-{index:02}"), serde_json::json!("value")));
    }
    let mut extensions = Some(extensions);

    assert!(bound_session_archive_extensions(&mut extensions));
    let bounded = extensions.expect("nonempty extensions should be retained");
    assert_eq!(bounded.len(), MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES);
    assert!(bounded.get("org.example.namespace-00").is_some());
    assert!(
        bounded
            .get(&format!("org.example.namespace-{:02}", MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES))
            .is_none()
    );
    assert!(serialized_json_size_at_most(&bounded, MAX_SESSION_ARCHIVE_EXTENSION_BYTES).is_some());

    let mut at_limit = SessionArchiveExtensions::new();
    drop(at_limit.insert("x", serde_json::Value::String("x".repeat(MAX_SESSION_ARCHIVE_EXTENSION_BYTES - 8))));
    let mut extensions = Some(at_limit);

    assert!(!bound_session_archive_extensions(&mut extensions));
    let bounded = extensions.expect("extension at the byte limit should be retained");
    assert_eq!(
        serialized_json_size_at_most(&bounded, MAX_SESSION_ARCHIVE_EXTENSION_BYTES),
        Some(MAX_SESSION_ARCHIVE_EXTENSION_BYTES)
    );

    let mut oversized = SessionArchiveExtensions::new();
    let _ =
        oversized.insert("a-oversized", serde_json::Value::String("x".repeat(MAX_SESSION_ARCHIVE_EXTENSION_BYTES * 2)));
    drop(oversized.insert("z-small", serde_json::json!("retained")));
    let mut extensions = Some(oversized);

    assert!(bound_session_archive_extensions(&mut extensions));
    let bounded = extensions.expect("metadata after dropping an oversized value should remain");
    assert_eq!(bounded.len(), 1);
    assert!(bounded.get("z-small").is_some());
    assert!(serialized_json_size_at_most(&bounded, MAX_SESSION_ARCHIVE_EXTENSION_BYTES).is_some());
}

#[test]
fn snapshot_compaction_reduces_extensions_to_fit_the_total_size_limit() -> Result<()> {
    let mut metadata =
        SessionArchiveMetadata::new("Workspace", "/tmp/workspace", "model", "provider", "dark", "medium");
    let mut extensions = SessionArchiveExtensions::new();
    for index in 0..MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES {
        drop(extensions.insert(format!("org.example.namespace-{index:02}"), serde_json::json!("value".repeat(40))));
    }
    metadata.extensions = Some(extensions);
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
            .extensions
            .is_none_or(|extensions| extensions.len() < MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES)
    );
    Ok(())
}

fn archive_json_value() -> impl Strategy<Value = serde_json::Value> {
    let scalar = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::Bool),
        any::<i16>().prop_map(|value| serde_json::json!(value)),
        "[a-zA-Z0-9 _-]{0,512}".prop_map(serde_json::Value::String),
    ];
    let array = prop::collection::vec(scalar.clone(), 0..5).prop_map(serde_json::Value::Array);
    let object = prop::collection::vec(("[a-z][a-z0-9_-]{0,8}", scalar.clone()), 0..5)
        .prop_map(|entries| serde_json::Value::Object(entries.into_iter().collect()));

    prop_oneof![scalar, array, object]
}

fn archive_extension_entries() -> impl Strategy<Value = BTreeMap<u8, serde_json::Value>> {
    prop::collection::btree_map(0_u8..120, archive_json_value(), 0..100)
}

fn reference_bounded_extensions(original: &SessionArchiveExtensions) -> Option<SessionArchiveExtensions> {
    let mut bounded = SessionArchiveExtensions::new();
    for (index, (namespace, value)) in original.iter().enumerate() {
        if index >= MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES {
            break;
        }
        drop(bounded.insert(namespace.clone(), value.clone()));
        if serde_json::to_vec(&bounded).expect("generated extensions serialize").len()
            > MAX_SESSION_ARCHIVE_EXTENSION_BYTES
        {
            drop(bounded.remove(namespace));
        }
    }

    if bounded.is_empty() {
        (original.is_empty()).then_some(bounded)
    } else {
        Some(bounded)
    }
}

proptest! {
    #[test]
    fn archive_extension_bounds_retain_deterministic_fitting_namespaces(
        entries in archive_extension_entries(),
    ) {
        let mut original = SessionArchiveExtensions::new();
        for (index, value) in entries {
            drop(original.insert(format!("org.example.namespace-{index:03}"), value));
        }

        let expected = reference_bounded_extensions(&original);
        let mut actual = Some(original.clone());
        let changed = bound_session_archive_extensions(&mut actual);
        prop_assert_eq!(&actual, &expected);
        prop_assert_eq!(changed, actual.as_ref() != Some(&original));

        if let Some(bounded) = &actual {
            prop_assert!(bounded.len() <= MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES);
            let serialized = serde_json::to_vec(bounded).expect("bounded extensions serialize");
            prop_assert!(serialized.len() <= MAX_SESSION_ARCHIVE_EXTENSION_BYTES);
        }

        let mut repeated = Some(original);
        let _ = bound_session_archive_extensions(&mut repeated);
        prop_assert_eq!(repeated, actual);
    }

    #[test]
    fn snapshot_compaction_obeys_every_generated_byte_budget(
        entries in archive_extension_entries(),
        payload_bytes in 0_usize..1_024,
        max_bytes in 0_usize..4_096,
    ) {
        let mut metadata = SessionArchiveMetadata::new(
            "Workspace",
            "/tmp/workspace",
            "model",
            "provider",
            "dark",
            "medium",
        );
        let mut extensions = SessionArchiveExtensions::new();
        for (index, value) in entries {
            drop(extensions.insert(format!("org.example.namespace-{index:03}"), value));
        }
        metadata.extensions = Some(extensions);

        let snapshot = SessionSnapshot {
            metadata,
            started_at: Utc::now(),
            ended_at: Utc::now(),
            total_messages: 1,
            distinct_tools: vec!["tool".repeat(payload_bytes / 8)],
            transcript: vec![
                "transcript ".repeat(payload_bytes),
                "✓│•—".repeat(payload_bytes / 4 + 1),
            ],
            messages: Vec::new(),
            progress: None,
            error_logs: Vec::new(),
        };

        match compact_snapshot_to_max_bytes(snapshot, max_bytes) {
            Ok(compacted) => {
                let serialized = serde_json::to_vec(&compacted).expect("compacted snapshot serializes");
                prop_assert!(
                    serialized.len() <= max_bytes,
                    "successful compaction returned {} bytes for a {max_bytes}-byte budget",
                    serialized.len(),
                );
            }
            Err(error) => prop_assert!(
                error.to_string().starts_with("session archive snapshot cannot fit the configured size limit after compaction:"),
                "unexpected compaction error: {error}"
            ),
        }
    }
}

#[test]
fn snapshot_compaction_truncates_multibyte_text_on_char_boundaries() -> Result<()> {
    let multibyte = "✓│•—".repeat(512);
    let snapshot = SessionSnapshot {
        metadata: SessionArchiveMetadata::new("Wörkspace", "/tmp/workspace", "mödel", "provider", "dark", "medium"),
        started_at: Utc::now(),
        ended_at: Utc::now(),
        total_messages: 1,
        distinct_tools: vec!["tööl".repeat(128)],
        transcript: vec![multibyte.clone()],
        messages: vec![SessionMessage::new(MessageRole::Assistant, multibyte)],
        progress: None,
        error_logs: Vec::new(),
    };

    match compact_snapshot_to_max_bytes(snapshot, 512) {
        Ok(compacted) => {
            let serialized = serde_json::to_vec(&compacted)?;
            assert!(serialized.len() <= 512);
        }
        Err(error) => assert!(
            error
                .to_string()
                .starts_with("session archive snapshot cannot fit the configured size limit after compaction:")
        ),
    }
    Ok(())
}

#[test]
fn zero_and_subminimum_snapshot_budgets_return_the_documented_size_error() {
    let snapshot = SessionSnapshot {
        metadata: SessionArchiveMetadata::new("Workspace", "/tmp/workspace", "model", "provider", "dark", "medium"),
        started_at: Utc::now(),
        ended_at: Utc::now(),
        total_messages: 0,
        distinct_tools: Vec::new(),
        transcript: Vec::new(),
        messages: Vec::new(),
        progress: None,
        error_logs: Vec::new(),
    };

    for max_bytes in [0, 1] {
        let error = compact_snapshot_to_max_bytes(snapshot.clone(), max_bytes)
            .expect_err("the required snapshot envelope cannot fit this budget");
        assert!(
            error
                .to_string()
                .starts_with("session archive snapshot cannot fit the configured size limit after compaction:"),
            "unexpected compaction error: {error}"
        );
    }
}

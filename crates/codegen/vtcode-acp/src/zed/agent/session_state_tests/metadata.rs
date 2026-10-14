//! Metadata behavior for ACP session state.
use super::*;
use crate::zed::agent::tool_execution::ToolAuditInput;
use assert_fs::TempDir;
use std::collections::BTreeSet;

async fn session_without_acp_metadata(agent: &ZedAgent, workspace: &Path) -> SessionHandle {
    let response = agent.new_session(acp::NewSessionRequest::new(workspace)).await.unwrap();
    let session = agent.session_handle(&response.session_id).unwrap().unwrap();
    let data = session.data.lock().unwrap();
    let mut metadata = data.thread.metadata().unwrap();
    metadata.extensions = None;
    data.thread.replace_metadata(Some(metadata));
    drop(data);
    session
}

#[tokio::test]
async fn session_metadata_captures_and_merges_acp_meta() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let creation_meta = acp::Meta::from_iter([
        ("client".to_string(), serde_json::json!("zed")),
        ("requestId".to_string(), serde_json::json!("create-1")),
    ]);

    let response = agent
        .new_session(acp::NewSessionRequest::new(temp.path()).meta(creation_meta))
        .await
        .unwrap();
    let session = agent.session_handle(&response.session_id).unwrap().unwrap();
    agent
        .merge_session_acp_meta(
            &session,
            Some(acp::Meta::from_iter([
                ("requestId".to_string(), serde_json::json!("prompt-1")),
                ("traceparent".to_string(), serde_json::json!("00-test-trace")),
            ])),
        )
        .unwrap();

    let metadata = session.data.lock().unwrap().thread.metadata().unwrap();
    let serialized = serde_json::to_value(metadata).unwrap();
    assert_eq!(serialized["extensions"][ACP_SESSION_METADATA_NAMESPACE]["client"], "zed");
    assert_eq!(serialized["extensions"][ACP_SESSION_METADATA_NAMESPACE]["requestId"], "prompt-1");
    assert_eq!(serialized["extensions"][ACP_SESSION_METADATA_NAMESPACE]["traceparent"], "00-test-trace");
}

#[tokio::test]
async fn session_metadata_records_absent_acp_meta_as_an_empty_object() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;

    let response = agent.new_session(acp::NewSessionRequest::new(temp.path())).await.unwrap();
    let session = agent.session_handle(&response.session_id).unwrap().unwrap();
    let metadata = session.data.lock().unwrap().thread.metadata().unwrap();
    let serialized = serde_json::to_value(metadata).unwrap();

    assert_eq!(serialized["extensions"][ACP_SESSION_METADATA_NAMESPACE], serde_json::json!({}));
}

#[tokio::test]
async fn session_metadata_merge_keeps_absent_acp_meta_absent() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session = session_without_acp_metadata(&agent, temp.path()).await;

    agent.merge_session_acp_meta(&session, None).unwrap();

    let metadata = session.data.lock().unwrap().thread.metadata().unwrap();
    assert!(metadata.extension(ACP_SESSION_METADATA_NAMESPACE).is_none());
}

#[tokio::test]
async fn session_metadata_merge_keeps_explicitly_empty_acp_meta_present() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session = session_without_acp_metadata(&agent, temp.path()).await;

    agent
        .merge_session_acp_meta(&session, Some(acp::Meta::from_iter(Vec::<(String, serde_json::Value)>::new())))
        .unwrap();

    let metadata = session.data.lock().unwrap().thread.metadata().unwrap();
    assert_eq!(metadata.extension(ACP_SESSION_METADATA_NAMESPACE), Some(&serde_json::json!({})));
}

#[test]
fn legacy_archive_acp_meta_is_migrated_at_the_protocol_boundary() {
    let mut metadata = serde_json::from_value::<SessionArchiveMetadata>(serde_json::json!({
        "workspace_label": "ws",
        "workspace_path": "/tmp/ws",
        "model": "model",
        "provider": "provider",
        "theme": "test",
        "reasoning_effort": "low",
        "acp_meta": {"requestId": "legacy-request"},
        "unrecognized_archive_value": {"kept": true}
    }))
    .expect("deserialize archive written by the earlier ACP field format");

    assert!(migrate_legacy_acp_meta(&mut metadata));
    assert_eq!(
        metadata.extension(ACP_SESSION_METADATA_NAMESPACE),
        Some(&serde_json::json!({"requestId": "legacy-request"}))
    );
    assert_eq!(
        metadata.extension(SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE),
        Some(&serde_json::json!({"unrecognized_archive_value": {"kept": true}}))
    );
}

#[test]
fn legacy_flat_client_metadata_is_migrated_at_the_protocol_boundary() {
    let mut metadata = serde_json::from_value::<SessionArchiveMetadata>(serde_json::json!({
        "workspace_label": "ws",
        "workspace_path": "/tmp/ws",
        "model": "model",
        "provider": "provider",
        "theme": "test",
        "reasoning_effort": "low",
        "client_metadata": {"requestId": "flat-request"}
    }))
    .expect("deserialize archive written by the flat metadata format");

    assert!(migrate_legacy_acp_meta(&mut metadata));
    assert_eq!(
        metadata.extension(ACP_SESSION_METADATA_NAMESPACE),
        Some(&serde_json::json!({"requestId": "flat-request"}))
    );
    assert!(metadata.extension(SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE).is_none());
}

#[tokio::test]
async fn tool_audit_uses_effective_arguments_and_the_session_selected_model() {
    let temp = TempDir::new().unwrap();
    let audit_path = temp.path().join("audit.jsonl");
    let audit_config: vtcode_config::safety::ToolAuditConfig =
        toml::from_str(&format!("enabled = true\npath = {:?}\nmax_size_bytes = 4096\nmax_files = 2", audit_path))
            .expect("audit configuration");
    let logger = crate::audit::AcpAuditLogger::from_config(&audit_config)
        .await
        .expect("audit logger startup")
        .expect("enabled audit logger");
    let mut agent = build_agent(temp.path()).await;
    agent.audit_logger = Some(Arc::new(logger));
    let response = agent.new_session(acp::NewSessionRequest::new(temp.path())).await.unwrap();
    let session = agent.session_handle(&response.session_id).unwrap().unwrap();
    session.data.lock().unwrap().model = "session-selected-model".to_string();

    let original = r#"{"path":"original.txt"}"#;
    let effective = serde_json::to_vec(&serde_json::json!({ "path": "approved.txt" })).unwrap();
    let call = vtcode_core::llm::provider::ToolCall::function_with_namespace(
        "call-1".to_string(),
        None,
        "read_file".to_string(),
        original.to_string(),
    );
    agent
        .record_tool_audit(ToolAuditInput {
            session: &session,
            session_id: &response.session_id,
            turn_id: "turn-1",
            call: &call,
            effective_arguments: &effective,
            result: Ok(("read", vtcode_safety::audit_log::ToolAuditStatus::Success)),
            duration_ms: 1,
        })
        .await;

    let line = fs::read_to_string(audit_path).unwrap();
    let entry: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(entry["arguments_hash"], vtcode_commons::utils::calculate_sha256(&effective));
    assert_ne!(entry["arguments_hash"], vtcode_commons::utils::calculate_sha256(original.as_bytes()));
    assert_eq!(entry["model_id"], "session-selected-model");
}

#[tokio::test]
async fn session_metadata_merge_bounds_entry_count_deterministically() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let response = agent.new_session(acp::NewSessionRequest::new(temp.path())).await.unwrap();
    let session = agent.session_handle(&response.session_id).unwrap().unwrap();
    let incoming = acp::Meta::from_iter(
        (0..=MAX_ACP_METADATA_ENTRIES).map(|index| (format!("key-{index:03}"), serde_json::json!(index))),
    );

    agent.merge_session_acp_meta(&session, Some(incoming)).unwrap();

    let data = session.data.lock().unwrap();
    let metadata = data.thread.metadata().unwrap();
    let metadata = metadata
        .extension(ACP_SESSION_METADATA_NAMESPACE)
        .and_then(serde_json::Value::as_object)
        .expect("bounded ACP metadata");
    let actual_keys = metadata.keys().cloned().collect::<BTreeSet<_>>();
    let expected_keys = (0..MAX_ACP_METADATA_ENTRIES)
        .map(|index| format!("key-{index:03}"))
        .collect::<BTreeSet<_>>();
    assert_eq!(actual_keys, expected_keys);
    assert!(serde_json::to_vec(&metadata).unwrap().len() <= MAX_SESSION_ARCHIVE_EXTENSION_BYTES);
}

#[tokio::test]
async fn session_metadata_merge_drops_oversized_entry_and_keeps_later_small_entry() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let response = agent.new_session(acp::NewSessionRequest::new(temp.path())).await.unwrap();
    let session = agent.session_handle(&response.session_id).unwrap().unwrap();
    let incoming = acp::Meta::from_iter([
        (
            "a-oversized".to_string(),
            serde_json::json!("x".repeat(MAX_SESSION_ARCHIVE_EXTENSION_BYTES.saturating_mul(64))),
        ),
        ("z-small".to_string(), serde_json::json!("retained")),
    ]);

    agent.merge_session_acp_meta(&session, Some(incoming)).unwrap();

    let data = session.data.lock().unwrap();
    let metadata = data.thread.metadata().unwrap();
    let metadata = metadata
        .extension(ACP_SESSION_METADATA_NAMESPACE)
        .and_then(serde_json::Value::as_object)
        .expect("bounded ACP metadata");
    assert_eq!(metadata, &serde_json::Map::from_iter([("z-small".to_string(), serde_json::json!("retained"))]));
    assert!(serde_json::to_vec(&metadata).unwrap().len() <= MAX_SESSION_ARCHIVE_EXTENSION_BYTES);
}

#[test]
fn oversized_metadata_size_probe_stops_at_the_configured_budget() {
    let metadata = serde_json::json!({"oversized": "x".repeat(MAX_SESSION_ARCHIVE_EXTENSION_BYTES.saturating_mul(64))});
    let mut writer = BoundedMetadataSizeWriter {
        bytes_written: 0,
        max_bytes: MAX_SESSION_ARCHIVE_EXTENSION_BYTES,
    };

    let result = serde_json::to_writer(&mut writer, &metadata);

    assert!(result.is_err(), "oversized metadata must stop serialization at the writer boundary");
    assert!(writer.bytes_written <= MAX_SESSION_ARCHIVE_EXTENSION_BYTES);
}

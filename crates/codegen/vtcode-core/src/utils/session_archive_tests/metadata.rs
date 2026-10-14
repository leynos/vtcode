use super::*;

#[cfg(test)]
mod metadata_compat_tests {
    use super::{SessionArchive, SessionArchiveMetadata};

    #[test]
    fn primary_agent_metadata_is_backward_compatible() {
        let metadata = serde_json::from_value::<SessionArchiveMetadata>(serde_json::json!({
            "workspace_label": "ws",
            "workspace_path": "/tmp/ws",
            "model": "gpt-5.6-sol",
            "provider": "openai",
            "theme": "test",
            "reasoning_effort": "low"
        }))
        .expect("deserialize metadata without primary_agent");

        assert_eq!(metadata.primary_agent, None);

        let value = serde_json::to_value(&metadata).expect("serialize metadata");
        assert!(value.get("primary_agent").is_none());
    }

    #[test]
    fn client_metadata_reads_legacy_acp_archive_field() {
        let metadata = serde_json::from_value::<SessionArchiveMetadata>(serde_json::json!({
            "workspace_label": "ws",
            "workspace_path": "/tmp/ws",
            "model": "gpt-5.6-sol",
            "provider": "openai",
            "theme": "test",
            "reasoning_effort": "low",
            "acp_meta": {"requestId": "legacy-request"}
        }))
        .expect("deserialize archive with legacy ACP metadata field");

        assert_eq!(metadata.client_metadata.as_ref().unwrap()["requestId"], "legacy-request");

        let value = serde_json::to_value(metadata).expect("serialize generic client metadata");
        assert_eq!(value["client_metadata"]["requestId"], "legacy-request");
        assert!(value.get("acp_meta").is_none());
    }

    #[test]
    fn set_primary_agent_records_mode_on_archive() {
        let metadata = SessionArchiveMetadata::new("ws", "/tmp/ws", "gpt-5.6-sol", "openai", "test", "low");
        let mut archive = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(SessionArchive::new_with_identifier(metadata, "ws-0000".to_string()))
            .expect("create archive");
        assert_eq!(archive.metadata().primary_agent, None);

        archive.set_primary_agent("plan");
        assert_eq!(archive.metadata().primary_agent.as_deref(), Some("plan"));
    }
}

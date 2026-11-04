//! Verifies archive metadata compatibility and bounded extension storage across serialization changes.
use super::*;

#[cfg(test)]
mod metadata_compat_tests {
    use super::{
        EnvGuard, SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE, SESSION_DIR_ENV, SessionArchive, SessionArchiveExtensions,
        SessionArchiveMetadata, lock_history_test_guard,
    };
    use anyhow::Result;

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
    fn namespaced_extensions_round_trip_as_opaque_values() {
        let mut metadata = SessionArchiveMetadata::new("ws", "/tmp/ws", "model", "provider", "test", "low");
        let mut extensions = SessionArchiveExtensions::new();
        drop(extensions.insert("org.example.client", serde_json::json!({"requestId": "request-1"})));
        metadata.extensions = Some(extensions);

        let value = serde_json::to_value(&metadata).expect("serialize namespaced extensions");
        assert_eq!(value["extensions"]["org.example.client"]["requestId"], "request-1");

        let restored = serde_json::from_value::<SessionArchiveMetadata>(value).expect("deserialize extensions");
        assert_eq!(restored, metadata);
    }

    #[test]
    fn unknown_archive_fields_are_preserved_in_a_generic_legacy_namespace() {
        let metadata = serde_json::from_value::<SessionArchiveMetadata>(serde_json::json!({
            "workspace_label": "ws",
            "workspace_path": "/tmp/ws",
            "model": "gpt-5.6-sol",
            "provider": "openai",
            "theme": "test",
            "reasoning_effort": "low",
            "vendor_archive_data": {"requestId": "legacy-request"}
        }))
        .expect("deserialize archive with an unrecognized field");

        assert_eq!(
            metadata.extension(SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE).unwrap()["vendor_archive_data"]["requestId"],
            "legacy-request"
        );
        let value = serde_json::to_value(metadata).expect("serialize generic archive extensions");
        assert_eq!(
            value["extensions"][SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE]["vendor_archive_data"]["requestId"],
            "legacy-request"
        );
    }

    #[serial_test::serial(session_dir_override)]
    #[tokio::test]
    async fn set_primary_agent_records_mode_on_archive() -> Result<()> {
        let _settings_lock = lock_history_test_guard().await;
        let temp_dir = tempfile::tempdir().expect("create temporary session directory");
        let _guard = EnvGuard::set(SESSION_DIR_ENV, temp_dir.path());

        let metadata = SessionArchiveMetadata::new("ws", "/tmp/ws", "gpt-5.6-sol", "openai", "test", "low");
        let mut archive = SessionArchive::new_with_identifier(metadata, "ws-0000".to_string()).await?;
        assert!(archive.path().starts_with(temp_dir.path()));
        assert_eq!(archive.metadata().primary_agent, None);

        archive.set_primary_agent("plan");
        assert_eq!(archive.metadata().primary_agent.as_deref(), Some("plan"));
        Ok(())
    }
}

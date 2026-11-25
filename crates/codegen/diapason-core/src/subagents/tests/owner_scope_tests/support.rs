//! Shared fixtures for the owner-scoped ledger and launch cases.
use super::*;
use crate::subagents::background::background_state_path_for_tests;

/// Creates the workspace state directory and returns the ledger path the
/// loader reads.
///
/// Tests build their fixtures through the same workspace-directory helper as
/// production, so an override of the workspace directory name cannot leave a
/// fixture ledger somewhere the loader never looks.
pub(super) fn ledger_path(workspace_root: &std::path::Path) -> PathBuf {
    let path = background_state_path_for_tests(workspace_root);
    std::fs::create_dir_all(path.parent().expect("ledger parent directory")).expect("state dir");
    path
}

/// Builds a persisted record with an explicit status and timestamp so a test
/// can order two writes to the same ledger entry.
pub(super) fn record_with_state(
    id: &str,
    owner_session_id: Option<&str>,
    status: &str,
    updated_at: &str,
) -> PersistedBackgroundRecord {
    let mut value = serde_json::to_value(persisted_background_record_for_owner(id, owner_session_id))
        .expect("serialize fixture record");
    value["status"] = serde_json::json!(status);
    value["updated_at"] = serde_json::json!(updated_at);
    serde_json::from_value(value).expect("deserialize record")
}

/// Builds a persisted record that names one launch's exec session.
///
/// A cancellation and a restart are told apart by the exec session they name,
/// so a test that exercises adoption has to say which launch each copy
/// describes.
pub(super) fn launch_record(
    desired_enabled: bool,
    status: &str,
    exec_session_id: &str,
    updated_at: &str,
) -> PersistedBackgroundRecord {
    let mut value = serde_json::to_value(persisted_background_record_for_owner("worker", Some("session-a")))
        .expect("serialize fixture record");
    value["desired_enabled"] = serde_json::json!(desired_enabled);
    value["status"] = serde_json::json!(status);
    value["exec_session_id"] = serde_json::json!(exec_session_id);
    value["updated_at"] = serde_json::json!(updated_at);
    value["ended_at"] = if status == "stopped" {
        serde_json::json!("2026-08-29T00:00:00Z")
    } else {
        serde_json::json!(null)
    };
    serde_json::from_value(value).expect("deserialize record")
}

pub(super) fn write_ledger(path: &std::path::Path, records: Vec<PersistedBackgroundRecord>) {
    std::fs::write(path, serde_json::to_string(&PersistedBackgroundState { records }).expect("serialize state"))
        .expect("write state");
}

pub(super) fn read_ledger_by_id(path: &std::path::Path) -> std::collections::HashMap<String, serde_json::Value> {
    let persisted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read persisted background_subagents state"))
            .expect("persisted state");
    let records = persisted["records"].as_array().expect("records array").iter();
    let mut by_id = std::collections::HashMap::new();
    for record in records {
        let id = record["id"].as_str().expect("record id").to_string();
        // A duplicate id would silently replace the earlier entry, so a test
        // asserting on that id would read the wrong record. The ledger keys one
        // record per canonical id, so a repeat means the merge wrote two.
        let previous = by_id.insert(id.clone(), record.clone());
        assert!(previous.is_none(), "the ledger must not hold two records under id {id}");
    }
    by_id
}

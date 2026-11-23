//! Pins that a superseded owned launch leaves no ownerless copy behind.
use super::super::launch::join_by_canonical_id;
use super::super::*;
use crate::subagents::background::background_record_id_for_owner;
use serde_json::json;

/// Builds one persisted record with the canonical id for its agent/owner.
fn persisted(
    agent: &str,
    owner_session_id: Option<&str>,
    exec_session_id: &str,
    updated_at: &str,
) -> PersistedBackgroundRecord {
    serde_json::from_value(json!({
        "id": background_record_id_for_owner(agent, owner_session_id),
        "agent_name": agent,
        "display_label": agent,
        "description": agent,
        "source": "builtin",
        "color": null,
        "owner_session_id": owner_session_id,
        "session_id": format!("session-{exec_session_id}"),
        "parent_session_id": owner_session_id.unwrap_or("parent-session"),
        "exec_session_id": exec_session_id,
        "desired_enabled": true,
        "status": "running",
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": updated_at,
        "started_at": null,
        "ended_at": null,
        "pid": null,
        "prompt": "work",
        "summary": null,
        "error": null,
        "archive_path": null,
        "transcript_path": null,
        "max_turns": null,
        "model_override": null,
        "reasoning_override": null,
        "restart_attempts": 0
    }))
    .expect("persisted background record")
}

/// A newer owned launch supersedes an older owned launch of the same agent.
///
/// The older launch's ownerless copy has no owned survivor left to match,
/// so collecting ownership evidence only from the reduced launches kept it
/// beside the replacement. The unrelated ownerless record is not evidence
/// of a migrated launch and must survive.
#[test]
fn an_ownerless_copy_of_a_superseded_owned_launch_is_dropped() {
    let ledger = vec![
        persisted("worker", None, "exec-1", "2026-08-28T00:00:00Z"),
        persisted("worker", Some("session-a"), "exec-1", "2026-08-28T00:00:00Z"),
        persisted("outside", None, "exec-outside", "2026-08-28T00:00:00Z"),
    ];
    let snapshot = vec![persisted("worker", Some("session-a"), "exec-2", "2026-08-29T00:00:00Z")];

    let merged = join_by_canonical_id(ledger, snapshot, &BTreeMap::new(), &[]);

    assert!(
        merged
            .values()
            .all(|record| !(record.owner_session_id.is_none() && record.exec_session_id == "exec-1")),
        "the ownerless copy of the superseded launch must not survive: {merged:?}"
    );
    assert!(
        merged
            .values()
            .any(|record| record.owner_session_id.is_none() && record.exec_session_id == "exec-outside"),
        "an ownerless record no owned copy names must survive: {merged:?}"
    );
    let owned = merged
        .values()
        .filter(|record| record.owner_session_id.as_deref() == Some("session-a"))
        .collect::<Vec<_>>();
    assert_eq!(owned.len(), 1, "the newer owned launch is the only owned survivor");
    assert_eq!(owned[0].exec_session_id, "exec-2");
}

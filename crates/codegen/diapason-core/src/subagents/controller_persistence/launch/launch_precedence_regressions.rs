//! Regressions for the launch copies a save keeps and the ones it discards.
//!
//! Declared by `launch.rs`. The fixtures build persisted copies directly, so a
//! rule change is measured on the merge rather than through a controller. Each
//! case pins one identity rule: a refreshed copy never resurrects a launch the
//! ledger stopped, an ineligible alias cannot win its canonical id, and an
//! identified launch survives the rows an ambiguous id cannot tell apart.

use super::*;

/// Builds one persisted copy under the canonical id for its agent and owner.
fn launch_copy(
    agent: &str,
    owner: Option<&str>,
    exec_session_id: &str,
    desired_enabled: bool,
    updated_at: &str,
) -> PersistedBackgroundRecord {
    let id = crate::subagents::background::background_record_id_for_owner(agent, owner);
    serde_json::from_value(serde_json::json!({
        "id": id,
        "owner_session_id": owner,
        "agent_name": agent,
        "display_label": agent,
        "description": agent,
        "source": "builtin",
        "color": null,
        "session_id": format!("session-{exec_session_id}"),
        "parent_session_id": owner.unwrap_or("parent-session"),
        "exec_session_id": exec_session_id,
        "desired_enabled": desired_enabled,
        "status": if desired_enabled { "running" } else { "stopped" },
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
    .unwrap_or_else(|error| panic!("persisted launch copy: {error}"))
}

/// A refresh of a stopped launch must not resurrect it.
///
/// The ledger holds a stopped copy of `exec-stopped` and a newer
/// `exec-current` under one canonical id, so the per-id reduction keeps the
/// current launch and drops the stop. A snapshot copy that merely refreshes
/// `exec-stopped` must not then win on `updated_at`; the persisted stop must
/// compete with it so the cancellation rule applies. A restart, which names
/// a new exec session, still supersedes the stopped launch.
#[test]
fn a_refreshed_copy_of_a_stopped_session_does_not_replace_the_persisted_launch() {
    let persisted = vec![
        launch_copy("worker", Some("session-a"), "exec-stopped", false, "2026-08-28T00:00:01Z"),
        launch_copy("worker", Some("session-a"), "exec-current", true, "2026-08-28T00:00:02Z"),
    ];
    let snapshot = vec![launch_copy(
        "worker",
        Some("session-a"),
        "exec-stopped",
        true,
        "2026-08-28T00:00:03Z",
    )];

    let merged = join_by_canonical_id(persisted, snapshot, &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1, "the canonical id keeps one launch");
    let record = merged.values().next().expect("the surviving launch");
    assert_eq!(
        record.exec_session_id, "exec-current",
        "a refresh of the stopped session must not replace the launch the ledger kept"
    );
    assert!(record.desired_enabled, "the surviving launch is still desired");
}

/// A restart with a new exec session still supersedes the stopped launch.
///
/// Retaining the persisted stop of a refreshed session must not block a
/// restart: it names a new exec session, so it matches no stopped copy and
/// the within-launch cancellation rule does not apply to it.
#[test]
fn a_restart_with_a_new_exec_session_supersedes_the_stopped_launch() {
    let persisted = vec![
        launch_copy("worker", Some("session-a"), "exec-stopped", false, "2026-08-28T00:00:01Z"),
        launch_copy("worker", Some("session-a"), "exec-current", true, "2026-08-28T00:00:02Z"),
    ];
    let snapshot = vec![launch_copy(
        "worker",
        Some("session-a"),
        "exec-restart",
        true,
        "2026-08-28T00:00:04Z",
    )];

    let merged = join_by_canonical_id(persisted, snapshot, &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1, "the canonical id keeps one launch");
    let record = merged.values().next().expect("the surviving launch");
    assert_eq!(
        record.exec_session_id, "exec-restart",
        "a restart names a new exec session and supersedes the launch it replaced"
    );
    assert!(record.desired_enabled, "the restarted launch is desired");
}

/// A migrated alias must not win its id and suppress an independent launch.
///
/// The ledger holds an independent ownerless `exec-workspace`, while the
/// snapshot still carries a cached ownerless copy of a launch whose
/// migration is recorded under `exec-migrated`. The alias has the later
/// timestamp, so it used to win the canonical-id reduction and then be
/// dropped, removing `exec-workspace` from the ledger with it.
#[test]
fn a_migrated_alias_does_not_suppress_an_independent_ownerless_launch() {
    let ledger = vec![launch_copy(
        "worker",
        None,
        "exec-workspace",
        true,
        "2026-08-28T00:00:01Z",
    )];
    let snapshot = vec![launch_copy(
        "worker",
        None,
        "exec-migrated",
        true,
        "2026-08-28T00:00:09Z",
    )];
    let evidence = vec![MigratedBackgroundLaunch {
        base_id: crate::subagents::background::background_record_id_for_owner("worker", None),
        exec_session_id: "exec-migrated".to_string(),
    }];

    let merged = join_by_canonical_id(ledger, snapshot, &BTreeMap::new(), &evidence);

    assert_eq!(merged.len(), 1, "the independent launch keeps the canonical id");
    let record = merged.values().next().expect("the independent launch");
    assert_eq!(
        record.exec_session_id, "exec-workspace",
        "a newer migrated alias must not suppress an independent ownerless launch"
    );
}

/// An empty exec session cannot shadow an ownerless row under an owned copy.
///
/// An ownerless `worker` row and a session-owned `worker` row can be different
/// child sessions and both lack an exec id. They have different canonical ids,
/// but their alias keys both collapse to `(background-worker, "")`, so the
/// ownership filter used to drop the ownerless row as if it were a stale copy of
/// the owned launch. A row that names no launch cannot be shown to be a copy of
/// one, so it must survive.
#[test]
fn an_empty_exec_session_does_not_shadow_an_ownerless_row() {
    let ownerless = launch_copy("worker", None, "", true, "2026-08-28T00:00:01Z");
    let owned = launch_copy("worker", Some("session-a"), "", true, "2026-08-28T00:00:02Z");

    let merged = collect_by_canonical_id(vec![ownerless, owned]);

    assert_eq!(
        merged.len(),
        2,
        "an ownerless row without a launch identity is not a copy of an owned one: {merged:?}"
    );
    assert!(
        merged.contains_key(&crate::subagents::background::background_record_id_for_owner("worker", None)),
        "the ownerless row survives beside the owned one"
    );
    assert!(
        merged.contains_key(&crate::subagents::background::background_record_id_for_owner("worker", Some("session-a"))),
        "the owned row is kept"
    );
}

/// Empty migration evidence cannot drop an ownerless row without a launch identity.
///
/// The migration evidence records an owned legacy row even when it carries no
/// exec session. An empty id is not evidence that an ownerless row is the copy
/// of that launch: without a session the two rows cannot be related, so the
/// ownerless row is kept rather than suppressed.
#[test]
fn empty_launch_evidence_does_not_drop_an_ownerless_row() {
    let ownerless = launch_copy("worker", None, "", true, "2026-08-28T00:00:01Z");
    let evidence = vec![MigratedBackgroundLaunch {
        base_id: crate::subagents::background::background_record_id_for_owner("worker", None),
        exec_session_id: String::new(),
    }];

    let merged = join_by_canonical_id(vec![ownerless], std::iter::empty(), &BTreeMap::new(), &evidence);

    assert_eq!(merged.len(), 1, "empty migration evidence must not suppress an ownerless row");
    let record = merged.values().next().expect("the ownerless row");
    assert!(record.exec_session_id.is_empty(), "the saved row keeps its identity");
}

/// An identified persisted launch survives empty rows under its canonical id.
///
/// Two rows with empty exec ids make their canonical id ambiguous, so neither
/// can be resolved. A third row under that id names `exec-live`, and that is an
/// identified launch: dropping the whole id discards a live task the load should
/// list and a save should keep.
#[test]
fn an_identified_launch_survives_empty_launch_rows_under_its_id() {
    let stopped = launch_copy("worker", Some("session-a"), "", false, "2026-08-28T00:00:01Z");
    let desired = launch_copy("worker", Some("session-a"), "", true, "2026-08-28T00:00:02Z");
    let live = launch_copy("worker", Some("session-a"), "exec-live", true, "2026-08-28T00:00:03Z");
    let records = vec![stopped, desired, live];

    let loaded = collect_by_canonical_id(records.clone());
    assert_eq!(loaded.len(), 1, "the canonical id keeps its identified launch");
    let record = loaded.values().next().expect("the identified launch");
    assert_eq!(
        record.exec_session_id, "exec-live",
        "an identified launch must not be discarded with the rows the id cannot tell apart"
    );

    // A save without a snapshot copy must keep it too, or the next load cannot
    // restore the live task.
    let merged = super::super::merge::merge_background_records(
        records,
        Vec::new(),
        Some("session-a"),
        &BTreeSet::new(),
        &BTreeMap::new(),
        &[],
    );
    assert_eq!(merged.len(), 1, "the save keeps the identified launch");
    assert_eq!(merged[0].exec_session_id, "exec-live");
}

/// Equal copies of one launch resolve by settlement, not by file order.
///
/// The two copies name the same launch, carry the same `updated_at` and agree on
/// intent; only the status they report differs. Arrival order alone decided
/// which one a load reported, so reversing the two rows changed the record a
/// caller saw. The settled copy is the outcome of that launch and must win
/// whichever row comes first.
#[test]
fn equal_launch_copies_resolve_by_settlement_in_either_order() {
    // Both copies still want the launch, so the intent rule cannot decide them
    // and only the status each reports is left.
    let mut settled = launch_copy("worker", Some("session-a"), "exec-tied", true, "2026-08-28T00:00:02Z");
    settled.status = crate::subagents::BackgroundSubprocessStatus::Stopped;
    let active = launch_copy("worker", Some("session-a"), "exec-tied", true, "2026-08-28T00:00:02Z");

    for records in [
        vec![active.clone(), settled.clone()],
        vec![settled.clone(), active.clone()],
    ] {
        let merged = join_by_canonical_id(records, std::iter::empty(), &BTreeMap::new(), &[]);
        assert_eq!(merged.len(), 1, "the canonical id keeps one copy of the launch");
        let record = merged.values().next().expect("the surviving copy");
        assert_eq!(
            record.status,
            crate::subagents::BackgroundSubprocessStatus::Stopped,
            "the settled copy must win whichever row comes first"
        );
    }
}

/// A strictly newer copy still outranks an older settled one.
///
/// Settlement only breaks a tie: freshness comes first, so a newer active copy
/// is not replaced by an older stopped one of the same launch.
#[test]
fn a_newer_active_copy_outranks_an_older_settled_copy() {
    // Both copies still want the launch, so only freshness separates them.
    let mut older_stopped = launch_copy("worker", Some("session-a"), "exec-tied", true, "2026-08-28T00:00:01Z");
    older_stopped.status = crate::subagents::BackgroundSubprocessStatus::Stopped;
    let newer_active = launch_copy("worker", Some("session-a"), "exec-tied", true, "2026-08-28T00:00:09Z");

    for records in [
        vec![older_stopped.clone(), newer_active.clone()],
        vec![newer_active.clone(), older_stopped.clone()],
    ] {
        let merged = join_by_canonical_id(records, std::iter::empty(), &BTreeMap::new(), &[]);
        let record = merged.values().next().expect("the surviving copy");
        assert_eq!(
            record.updated_at,
            "2026-08-28T00:00:09Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .expect("the newer timestamp"),
            "a strictly newer copy must not lose to an older settled one"
        );
    }
}

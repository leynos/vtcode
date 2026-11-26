//! Precedence between a snapshot launch and the launch the ledger holds.
//!
//! Declared by `controller_persistence/tests/mod.rs`. Pins that a refreshed or
//! stopped copy yields to a launch it does not name, that a launch this save
//! started supersedes the one it replaced, and that durable migration evidence
//! suppresses a stale ownerless copy. It reuses the record builders from
//! `merge_oracle_tests.rs` and the workspace fixtures from `adoption_tests.rs`.
use super::super::*;
use super::adoption_tests::{legacy_row, ownership_controller_config};
use super::merge_oracle_tests::{merge_canonical_id, merge_launch_record, merge_state};
use crate::subagents::background::{load_background_state, persist_background_state};
use crate::subagents::{BackgroundRecord, SubagentController, TaskOwner};
use std::path::Path;
use tempfile::TempDir;

/// An equal timestamp keeps the snapshot launch across launches too.
///
/// The tie rule is documented for two copies of one launch, but the same two
/// sides compete when a restart leaves two launches under one canonical id.
/// Without the snapshot's provenance the reduction kept whichever exec session
/// sorted last, so a persisted `exec-z` displaced an equal snapshot `exec-a`.
#[test]
fn an_equal_timestamp_keeps_the_snapshot_launch() {
    // Shape 0 is `worker` owned by `session-a`; both copies name the same
    // `updated_at`, and the persisted side sorts last by exec session id.
    let persisted = vec![merge_launch_record(0, 2, true, "exec-z", "persisted-exec-z")];
    let snapshot = vec![merge_launch_record(0, 2, true, "exec-a", "snapshot-exec-a")];

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1, "the two copies describe one canonical record");
    assert_eq!(
        merged[0].exec_session_id, "exec-a",
        "a tie must keep the snapshot launch instead of the later exec-session id"
    );
    assert_eq!(
        merge_state(&merged[0]),
        ("snapshot-exec-a".to_owned(), true, "running".to_owned()),
        "the snapshot copy wins the tie"
    );
}

/// A stale stop must not erase a replacement launch another controller wrote.
///
/// The stop names the launch it stopped, so stop precedence applies only on a
/// matching exec session. A persisted row naming a different launch under the
/// same canonical id is a replacement, and it survives even though the stale
/// stop carries the newer `updated_at`; otherwise a save from a controller
/// that never saw the replacement would delete the replacement launch from the
/// ledger.
#[test]
fn a_stale_stop_does_not_replace_a_different_launch() {
    // Shape 0 is `worker` owned by `session-a`.
    let persisted = vec![merge_launch_record(0, 1, true, "exec-replacement", "replacement")];
    let snapshot = vec![merge_launch_record(0, 3, false, "exec-stopped", "stale-stop")];
    // The save is stopping `exec-stopped`, the launch the snapshot names.
    let expected_stops = BTreeMap::from([(merge_canonical_id(0), "exec-stopped".to_string())]);

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &expected_stops, &[]);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].exec_session_id, "exec-replacement", "the replacement launch keeps the ledger row");
    assert_eq!(
        merge_state(&merged[0]),
        ("replacement".to_owned(), true, "running".to_owned()),
        "the stale stop must not overwrite a launch it does not name"
    );
}

/// A refreshed earlier launch must not displace a persisted replacement.
///
/// A refresh advances `updated_at` while it keeps the exec session it is
/// refreshing; the persisted ledger row names a later launch under the same
/// canonical id. Treating the newer `updated_at` as proof of supersession
/// overwrote the live replacement with the ended launch, and the next load then
/// reaped the replacement and restored the older process. Only a launch of this
/// save may supersede a different persisted session.
#[test]
fn a_refresh_of_an_earlier_launch_does_not_displace_a_persisted_replacement() {
    // Shape 0 is `worker` owned by `session-a`.
    let mut replacement = merge_launch_record(0, 1, true, "exec-replacement", "replacement");
    replacement.started_at = Some("2026-08-28T00:00:02Z".parse().expect("replacement launch time"));
    let persisted = vec![replacement];
    // The snapshot refreshes `exec-earlier`, a launch that began before the
    // replacement, long after the replacement was written: `updated_at` alone
    // would let the ended launch win.
    let mut earlier = merge_launch_record(0, 9, true, "exec-earlier", "refreshed-earlier");
    earlier.started_at = Some("2026-08-28T00:00:01Z".parse().expect("earlier launch time"));
    let snapshot = vec![earlier];

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1, "the record id keeps one launch");
    assert_eq!(
        merged[0].exec_session_id, "exec-replacement",
        "a refreshed copy of an earlier launch must not displace the persisted replacement"
    );
    assert_eq!(merge_state(&merged[0]).0, "replacement", "the replacement keeps the ledger row");
}

/// A stop applies to the current persisted launch, not an older colliding row.
///
/// Owner migration can leave an older and a newer enabled launch under one
/// canonical id. The load selects the newer one, so a stop of that launch is a
/// stop of the current row; treating the older row as a replacement launch
/// discarded the stop and made the older launch the enabled record for the next
/// load.
#[test]
fn a_stop_applies_to_the_current_persisted_launch() {
    // Shape 0 is `worker` owned by `session-a`.
    let persisted = vec![
        merge_launch_record(0, 1, true, "exec-1", "older-launch"),
        merge_launch_record(0, 2, true, "exec-2", "current-launch"),
    ];
    // The save stops `exec-2`, the launch a load selects.
    let snapshot = vec![merge_launch_record(0, 3, false, "exec-2", "stopped-current")];
    let expected_stops = BTreeMap::from([(merge_canonical_id(0), "exec-2".to_string())]);

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &expected_stops, &[]);

    assert_eq!(merged.len(), 1, "the record id keeps one launch");
    assert_eq!(
        merge_state(&merged[0]),
        ("stopped-current".to_owned(), false, "stopped".to_owned()),
        "a stop of the current launch must survive an older colliding row"
    );
    assert_eq!(merged[0].exec_session_id, "exec-2", "the older launch must not become the enabled row");
}

/// A settled snapshot copy that never started cannot displace a persisted launch.
///
/// A settlement with no `started_at` describes a launch that never ran: an
/// abandoned claim, or a run that failed before its process began. Its
/// `updated_at` is therefore not proof that it superseded anything, and using it
/// to win would overwrite a launch another controller has persisted and may be
/// running. The persisted replacement must survive.
///
/// The claim still wants the launch in this fixture, so the stop expectation
/// protects nothing here: only the missing start instant distinguishes it from a
/// launch of its own.
#[test]
fn a_settled_snapshot_copy_without_a_start_does_not_displace_a_persisted_launch() {
    // Shape 0 is `worker` owned by `session-a`; the snapshot's copy settled and
    // never recorded a launch instant, while its `updated_at` is the newer one.
    let persisted = vec![merge_launch_record(0, 1, true, "exec-live", "live-launch")];
    let mut settled = merge_launch_record(0, 9, true, "exec-never-started", "abandoned-claim");
    settled.started_at = None;
    settled.status = crate::subagents::BackgroundSubprocessStatus::Error;

    let merged =
        merge_background_records(persisted, vec![settled], Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1, "the canonical id keeps one launch");
    assert_eq!(
        merged[0].exec_session_id, "exec-live",
        "a launch that never started must not displace the persisted launch"
    );
    assert!(merged[0].desired_enabled, "the persisted launch keeps its intent");
}

/// A started copy still supersedes the persisted launch whatever its status.
///
/// The start instant is evidence of a launch of its own, so a copy that carries
/// one is compared by timestamp as before even when it has already settled: an
/// explicit restart that ran and stopped is the newer state of the record.
#[test]
fn a_started_snapshot_copy_still_supersedes_the_persisted_launch() {
    // Shape 0 is `worker` owned by `session-a`.
    let persisted = vec![merge_launch_record(0, 1, true, "exec-old", "old-launch")];
    let mut restarted = merge_launch_record(0, 9, false, "exec-restarted", "restarted-then-stopped");
    restarted.started_at = Some("2026-08-28T00:00:08Z".parse().expect("the restart instant"));

    let merged = merge_background_records(
        persisted,
        vec![restarted],
        Some("session-a"),
        &BTreeSet::new(),
        &BTreeMap::new(),
        &[],
    );

    assert_eq!(merged.len(), 1, "the canonical id keeps one launch");
    assert_eq!(
        merged[0].exec_session_id, "exec-restarted",
        "a launch that recorded its start supersedes the one it replaced"
    );
}

/// Writes a ledger whose `migrated_launches` evidence already names each
/// `(base id, exec session)` pair.
///
/// The fixture writes that evidence directly rather than running a migration:
/// these cases test consuming evidence that is already durable, which is the
/// state the core save leaves behind in production.
async fn write_ledger_with_existing_migration_evidence(
    workspace: &Path,
    records: Vec<PersistedBackgroundRecord>,
    migrated_launches: &[(&str, &str)],
) {
    let path = crate::subagents::background::background_state_path_for_tests(workspace);
    let evidence = migrated_launches
        .iter()
        .map(|(base_id, exec_session_id)| serde_json::json!({ "base_id": base_id, "exec_session_id": exec_session_id }))
        .collect::<Vec<_>>();
    let state = serde_json::json!({ "records": records, "migrated_launches": evidence });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create the ledger directory");
    }
    std::fs::write(&path, serde_json::to_string_pretty(&state).expect("serialize the ledger"))
        .expect("write the ledger");
}

/// Consuming existing migration evidence suppresses a stale workspace copy.
///
/// The workspace controller caches the ownerless launch before a session
/// migrates it. The session then replaces that launch, so the ledger no longer
/// holds any row naming the cached exec session, and a later save of the
/// unchanged workspace snapshot would write an ownerless row the next workspace
/// controller treats as its own work to restore. The migration evidence kept in
/// the ledger is what identifies the launch once its owned row is gone.
///
/// The fixture writes that evidence directly, which is the state the core save
/// leaves behind in production; this test is about consuming it, not about
/// migration recording it.
#[tokio::test]
async fn a_migrated_launch_is_not_revived_by_a_workspace_save_after_a_replacement() {
    let temp = TempDir::new().expect("temporary workspace");
    let owner = "session-a";
    persist_background_state(temp.path(), vec![legacy_row(owner)])
        .await
        .expect("seed the ownerless ledger");

    // The workspace controller caches the ownerless copy, exactly as the ACP
    // launch workspace does before any session attaches.
    let workspace_controller = SubagentController::new(ownership_controller_config(temp.path().to_path_buf()))
        .await
        .expect("workspace controller");
    assert_eq!(
        workspace_controller.background_status_entries(TaskOwner::Workspace).await.len(),
        1,
        "the workspace controller loads the ownerless launch"
    );

    // A session migrates the row and then replaces the launch it names, so the
    // ledger holds only the replacement. Its migration evidence names the
    // launch that was attributed, which the core save writes in production and
    // this fixture writes directly.
    let mut replacement = legacy_row(owner);
    replacement.owner_session_id = Some(owner.to_string());
    replacement.exec_session_id = "exec-replacement".to_string();
    replacement.canonicalize_id();
    write_ledger_with_existing_migration_evidence(
        temp.path(),
        vec![replacement],
        &[("background-worker", "exec-launch")],
    )
    .await;

    // The stale snapshot is saved again with nothing about it changed.
    workspace_controller
        .save_background_ledger()
        .await
        .expect("save the unchanged workspace snapshot");

    let ledger = load_background_state(temp.path()).await.expect("read the ledger");
    assert_eq!(ledger.records.len(), 1, "the stale ownerless alias must not reappear");
    assert_eq!(ledger.records[0].exec_session_id, "exec-replacement", "the replacement keeps the ledger row");
    assert_eq!(ledger.records[0].owner_session_id.as_deref(), Some(owner), "the row stays owned");
}

/// An independent ownerless launch of the same agent still survives.
///
/// Migration evidence names one launch by its exec session, so a second
/// ownerless launch of the same agent is a different launch the workspace may
/// restore, and the evidence must not suppress it.
#[tokio::test]
async fn migration_evidence_does_not_suppress_an_independent_ownerless_launch() {
    let temp = TempDir::new().expect("temporary workspace");
    let owner = "session-a";
    let mut migrated = legacy_row(owner);
    migrated.owner_session_id = Some(owner.to_string());
    migrated.canonicalize_id();
    // The independent launch carries the same ownerless base id and agent but a
    // different exec session, which is what distinguishes it from the migrated
    // launch.
    let mut independent = legacy_row(owner);
    independent.exec_session_id = "exec-independent".to_string();
    persist_background_state(temp.path(), vec![migrated])
        .await
        .expect("seed the migrated ledger");

    let workspace_controller = SubagentController::new(ownership_controller_config(temp.path().to_path_buf()))
        .await
        .expect("workspace controller");
    // Record the migration evidence, then save the independent launch.
    workspace_controller
        .save_background_ledger()
        .await
        .expect("save the migrated row");
    let independent_id = independent.id.clone();
    let independent_record = BackgroundRecord::from_persisted(independent);
    let mut state = workspace_controller.state.write().await;
    state.background_children.insert(independent_id, independent_record);
    drop(state);
    workspace_controller
        .save_background_ledger()
        .await
        .expect("save the independent launch");

    let ledger = load_background_state(temp.path()).await.expect("read the ledger");
    assert!(
        ledger.records.iter().any(|record| record.exec_session_id == "exec-independent"),
        "an independent ownerless launch must not be suppressed by migration evidence: {ledger:?}"
    );
}

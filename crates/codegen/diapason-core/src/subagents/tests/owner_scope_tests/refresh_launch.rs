//! Owner-scoped refresh adoption, managed launch and record-id tests.
use super::support::{launch_record, ledger_path, write_ledger};
use super::*;

/// A stop another controller persisted must survive this controller's refresh.
///
/// An owner-scoped controller keeps its own copy of the ledger, and an
/// unscoped controller shares that ledger across sessions, so the unscoped one
/// can cancel this owner's task and persist `desired_enabled = false` after
/// this controller loaded its copy. Refresh must adopt that intent before it
/// selects a restart: otherwise it would settle the exited process from the
/// stale flag, schedule a replacement, and save a newer copy that undoes the
/// stop.
#[tokio::test]
async fn refresh_adopts_a_cancellation_persisted_by_another_controller() {
    let temp = TempDir::new().expect("tempdir");
    let ledger = ledger_path(temp.path());
    write_ledger(&ledger, vec![launch_record(true, "running", "exec-launch-1", "2026-08-28T00:00:00Z")]);
    let mut cfg = DiapasonConfig::default();
    cfg.subagents.background.auto_restore = true;
    let controller = SubagentController::new_with_background_owner(
        test_controller_config(temp.path().to_path_buf(), cfg),
        Some("session-a"),
    )
    .await
    .expect("controller");

    // The shared controller stops the task after this one took its snapshot.
    write_ledger(&ledger, vec![launch_record(false, "stopped", "exec-launch-1", "2026-08-29T00:00:00Z")]);

    controller
        .refresh_background_processes()
        .await
        .expect("refresh must keep the persisted stop instead of respawning the task");

    let entry = controller
        .background_status_entries(TaskOwner::Workspace)
        .await
        .into_iter()
        .find(|entry| entry.id == "background-worker@session-a")
        .expect("owned background record");
    assert!(!entry.desired_enabled, "refresh must not resurrect a task another controller stopped");
    assert_eq!(
        entry.status,
        BackgroundSubprocessStatus::Stopped,
        "the adopted stop keeps the terminal outcome the cancellation recorded"
    );
}

/// A stop that names the previous launch must not cancel its replacement.
///
/// The exec session identifies the launch, so a cancellation written for the
/// session that was stopped cannot apply to a record that already names a new
/// session. Adopting it would silently cancel an explicit restart.
#[tokio::test]
async fn refresh_does_not_adopt_a_cancellation_for_an_older_launch() {
    let temp = TempDir::new().expect("tempdir");
    let ledger = ledger_path(temp.path());
    write_ledger(&ledger, vec![launch_record(true, "running", "exec-launch-2", "2026-08-28T00:00:00Z")]);
    // No restart is attempted, so the refresh only reports what it observed.
    let mut cfg = DiapasonConfig::default();
    cfg.subagents.background.auto_restore = false;
    let controller = SubagentController::new_with_background_owner(
        test_controller_config(temp.path().to_path_buf(), cfg),
        Some("session-a"),
    )
    .await
    .expect("controller");

    // The cancellation names the launch this record has already replaced.
    write_ledger(&ledger, vec![launch_record(false, "stopped", "exec-launch-1", "2026-08-29T00:00:00Z")]);

    controller.refresh_background_processes().await.expect("refresh");

    let entry = controller
        .background_status_entries(TaskOwner::Workspace)
        .await
        .into_iter()
        .find(|entry| entry.id == "background-worker@session-a")
        .expect("owned background record");
    assert!(entry.desired_enabled, "a stop for the previous launch must not cancel the launch that replaced it");
}

#[cfg(unix)]
async fn spawn_background_demo(owner_session_id: Option<&str>) -> BackgroundSubprocessEntry {
    let temp = TempDir::new().expect("tempdir");
    write_test_background_subagent(temp.path());
    let mut cfg = DiapasonConfig::default();
    cfg.subagents.background.enabled = true;
    let controller = SubagentController::new_with_background_owner(
        test_controller_config(temp.path().to_path_buf(), cfg),
        owner_session_id,
    )
    .await
    .expect("controller");

    controller
        .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
            agent_type: Some("background-demo".to_string()),
            ..SpawnBackgroundSubprocessRequest::default()
        })
        .await
        .expect("background subprocess")
}

#[cfg(unix)]
#[tokio::test]
async fn managed_background_records_capture_the_owning_scope() {
    let scoped = spawn_background_demo(Some("session-a")).await;
    assert_eq!(scoped.owner_session_id.as_deref(), Some("session-a"));
    assert_eq!(scoped.id, "background-background-demo@session-a");

    let unscoped = spawn_background_demo(None).await;
    assert!(
        unscoped.owner_session_id.is_none(),
        "an unscoped controller manages workspace-global records and must not tag them with a per-run owner"
    );
    assert_eq!(unscoped.id, "background-background-demo");
}

#[test]
fn owner_scoped_record_ids_distinguish_owners_and_keep_the_global_id() {
    assert_eq!(background_record_id_for_owner("worker", Some("session-a")), "background-worker@session-a");
    assert_ne!(
        background_record_id_for_owner("worker", Some("session-a")),
        background_record_id_for_owner("worker", Some("session-b"))
    );
    assert_eq!(background_record_id_for_owner("worker", None), background_record_id("worker"));
    assert_eq!(
        background_record_id_for_owner("worker", Some("")),
        background_record_id("worker"),
        "an empty owner cannot produce a distinct scope"
    );
}

/// The owner component must be injective: a display-oriented sanitizer mapped
/// `:`, `/`, `.`, spaces and surrounding hyphens all onto `-`, so distinct
/// owners collapsed into one record id. The unscoped load path keys a
/// `BTreeMap` by that id, so the collision dropped a record and the next save
/// rewrote the ledger without it.
#[test]
fn owner_record_ids_are_injective_for_unsafe_owner_characters() {
    let unsafe_owners = ["session:a", "session/a", "session.a", "session a", "-session-a-"];
    let ids = unsafe_owners
        .iter()
        .map(|owner| background_record_id_for_owner("agent", Some(owner)))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), unsafe_owners.len(), "distinct owners must produce distinct record ids: {ids:?}");

    assert_eq!(
        background_record_id_for_owner("agent", Some("session-a")),
        "background-agent@session-a",
        "an owner made only of safe characters maps to itself"
    );
}

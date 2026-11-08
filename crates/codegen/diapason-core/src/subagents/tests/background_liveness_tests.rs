//! The read-only liveness projection of a background listing must follow the
//! record's restart budget and never mutate the persisted ledger.
use super::*;

/// Builds a persisted record whose exec session is not registered with the
/// controller, so the liveness overlay applies, and overrides the cached
/// status and restart budget the projection consults.
fn record_with_restart_state(
    id: &str,
    owner_session_id: Option<&str>,
    status: &str,
    restart_attempts: u8,
) -> PersistedBackgroundRecord {
    let mut value = serde_json::to_value(persisted_background_record_for_owner(id, owner_session_id))
        .expect("serialize fixture record");
    value["status"] = serde_json::json!(status);
    value["desired_enabled"] = serde_json::json!(true);
    value["restart_attempts"] = serde_json::json!(restart_attempts);
    serde_json::from_value(value).expect("deserialize record")
}

fn write_ledger(path: &std::path::Path, records: Vec<PersistedBackgroundRecord>) {
    std::fs::write(path, serde_json::to_string(&PersistedBackgroundState { records }).expect("serialize state"))
        .expect("write state");
}

fn auto_restore_config(workspace_root: PathBuf, auto_restore: bool) -> SubagentControllerConfig {
    let mut cfg = DiapasonConfig::default();
    cfg.subagents.background.auto_restore = auto_restore;
    test_controller_config(workspace_root, cfg)
}

/// Projects one persisted record through the liveness query and returns the
/// status the caller would observe.
async fn projected_liveness_status(
    temp: &TempDir,
    record: PersistedBackgroundRecord,
    auto_restore: bool,
) -> BackgroundSubprocessStatus {
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    write_ledger(&state_dir.join("background_subagents.json"), vec![record]);
    let controller = SubagentController::new_with_background_owner(
        auto_restore_config(temp.path().to_path_buf(), auto_restore),
        Some("session-a"),
    )
    .await
    .expect("controller");

    controller
        .background_status_entries_with_liveness()
        .await
        .into_iter()
        .find(|entry| entry.id == "background-worker@session-a")
        .expect("owned background record")
        .status
}

/// A record that has already spent its one restart is reported as the error a
/// refresh would settle it as, not as a restart that will never happen.
#[tokio::test]
async fn exhausted_restart_budget_projects_error() {
    let temp = TempDir::new().expect("tempdir");
    let status = projected_liveness_status(
        &temp,
        record_with_restart_state("background-worker", Some("session-a"), "running", 1),
        true,
    )
    .await;

    assert_eq!(status, BackgroundSubprocessStatus::Error);
}

/// The restart count is incremented before the replacement exec session is
/// registered, so a cached `Starting` record with a spent budget is a restart
/// still in progress and must keep reporting `Starting`.
#[tokio::test]
async fn restart_in_progress_projects_starting() {
    let temp = TempDir::new().expect("tempdir");
    let status = projected_liveness_status(
        &temp,
        record_with_restart_state("background-worker", Some("session-a"), "starting", 1),
        true,
    )
    .await;

    assert_eq!(status, BackgroundSubprocessStatus::Starting);
}

/// A record that has not yet spent its restart allowance is about to be
/// restarted, so an exited process is projected as `Starting`.
#[tokio::test]
async fn available_restart_budget_projects_starting() {
    let temp = TempDir::new().expect("tempdir");
    let status = projected_liveness_status(
        &temp,
        record_with_restart_state("background-worker", Some("session-a"), "running", 0),
        true,
    )
    .await;

    assert_eq!(status, BackgroundSubprocessStatus::Starting);
}

/// The liveness query is a projection: it must not settle the cached record,
/// rewrite the ledger, or otherwise behave like a refresh.
#[tokio::test]
async fn liveness_projection_does_not_mutate_the_ledger() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let ledger_path = state_dir.join("background_subagents.json");
    write_ledger(
        &ledger_path,
        vec![record_with_restart_state(
            "background-worker",
            Some("session-a"),
            "running",
            1,
        )],
    );

    let controller = SubagentController::new_with_background_owner(
        auto_restore_config(temp.path().to_path_buf(), true),
        Some("session-a"),
    )
    .await
    .expect("controller");
    let ledger_before = std::fs::read(&ledger_path).expect("read ledger before query");

    let projected = controller.background_status_entries_with_liveness().await;
    assert_eq!(
        projected.iter().map(|entry| entry.status).collect::<Vec<_>>(),
        [BackgroundSubprocessStatus::Error],
        "the projection reports the settled outcome of an exhausted restart budget"
    );

    let cached = controller.background_status_entries().await;
    assert_eq!(
        cached.iter().map(|entry| entry.status).collect::<Vec<_>>(),
        [BackgroundSubprocessStatus::Running],
        "the liveness query must not settle the cached record"
    );
    assert_eq!(
        std::fs::read(&ledger_path).expect("read ledger after query"),
        ledger_before,
        "the liveness query must not rewrite the persisted ledger"
    );
}

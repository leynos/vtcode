//! The read-only liveness projection of a background listing must follow the
//! record's restart budget, report a terminal instant, and never mutate the
//! persisted ledger. A launch that never started must settle as the failure it
//! is rather than remain `Starting` forever.
use super::*;
use rstest::rstest;

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

/// Overrides the projection's terminal-time inputs on the fixture record:
/// whether the owner still desires the process, and whether a terminal instant
/// was ever observed.
fn record_with_terminal_state(
    id: &str,
    owner_session_id: Option<&str>,
    status: &str,
    restart_attempts: u8,
    desired_enabled: bool,
    ended_at: Option<&str>,
) -> PersistedBackgroundRecord {
    let mut value = serde_json::to_value(record_with_restart_state(id, owner_session_id, status, restart_attempts))
        .expect("serialize fixture record");
    value["desired_enabled"] = serde_json::json!(desired_enabled);
    value["ended_at"] = serde_json::json!(ended_at);
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
/// entry the caller would observe.
async fn projected_liveness_entry(
    temp: &TempDir,
    record: PersistedBackgroundRecord,
    auto_restore: bool,
) -> BackgroundSubprocessEntry {
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
        .find(|observed| observed.entry.id == "background-worker@session-a")
        .expect("owned background record")
        .entry
}

/// Projects one persisted record through the liveness query and returns the
/// status the caller would observe.
async fn projected_liveness_status(
    temp: &TempDir,
    record: PersistedBackgroundRecord,
    auto_restore: bool,
) -> BackgroundSubprocessStatus {
    projected_liveness_entry(temp, record, auto_restore).await.status
}

/// The liveness overlay follows the record's restart budget.
///
/// A record that has already spent its one restart is reported as the error a
/// refresh would settle it as, not as a restart that will never happen. The
/// restart count is incremented before the replacement exec session is
/// registered, so a cached `Starting` record with a spent budget is a restart
/// still in progress and keeps reporting `Starting`. A record that has not yet
/// spent its allowance is about to be restarted, so an exited process is also
/// projected as `Starting`. A `Starting` record is preserved regardless of the
/// restart budget or `auto_restore`: its exec session is not registered yet, so
/// a failed projection would report a merely starting task as an error.
#[rstest]
#[case::exhausted_budget("running", 1, true, BackgroundSubprocessStatus::Error)]
#[case::restart_in_progress("starting", 1, true, BackgroundSubprocessStatus::Starting)]
#[case::available_budget("running", 0, true, BackgroundSubprocessStatus::Starting)]
#[case::starting_without_auto_restore("starting", 0, false, BackgroundSubprocessStatus::Starting)]
#[tokio::test]
async fn liveness_projection_follows_the_restart_budget(
    #[case] cached_status: &str,
    #[case] restart_attempts: u8,
    #[case] auto_restore: bool,
    #[case] expected: BackgroundSubprocessStatus,
) {
    let temp = TempDir::new().expect("tempdir");
    let status = projected_liveness_status(
        &temp,
        record_with_restart_state("background-worker", Some("session-a"), cached_status, restart_attempts),
        auto_restore,
    )
    .await;

    assert_eq!(
        status, expected,
        "cached {cached_status} record with {restart_attempts} restart(s) already spent and auto_restore {auto_restore}"
    );
}

/// A projection that settles a terminal status must report a terminal instant.
///
/// The ACP wire contract numbers `endedAtEpochSeconds` for a terminal task and
/// leaves it null only while the task is active. A record whose process exited
/// before any end instant was observed is projected as the error a refresh
/// would settle it as, so the projection supplies the record's own last
/// observed instant. Using that stored instant, rather than the query time,
/// keeps repeated polls of one unchanged record in agreement.
#[tokio::test]
async fn terminal_liveness_projection_reports_the_last_observed_instant() {
    let temp = TempDir::new().expect("tempdir");
    let last_observed: chrono::DateTime<Utc> = "2026-08-28T00:00:00Z".parse().expect("fixture instant");
    let entry = projected_liveness_entry(
        &temp,
        record_with_terminal_state("background-worker", Some("session-a"), "running", 1, true, None),
        false,
    )
    .await;

    assert_eq!(
        entry.status,
        BackgroundSubprocessStatus::Error,
        "an exited process with a spent restart budget is a failure"
    );
    assert_eq!(entry.updated_at, last_observed);
    assert_eq!(
        entry.ended_at,
        Some(last_observed),
        "a terminal projection must report the record's last observed instant as its end"
    );
}

/// A projection that keeps a task active leaves its end timestamp unset, so a
/// client never reports an active task as already terminal.
#[tokio::test]
async fn active_liveness_projection_leaves_the_end_timestamp_unset() {
    let temp = TempDir::new().expect("tempdir");
    let entry = projected_liveness_entry(
        &temp,
        record_with_terminal_state("background-worker", Some("session-a"), "running", 0, true, None),
        true,
    )
    .await;

    assert_eq!(entry.status, BackgroundSubprocessStatus::Starting);
    assert_eq!(entry.ended_at, None, "an active task has no terminal instant");
}

/// A stopped projection also carries the terminal instant: the owner cleared
/// `desired_enabled`, so the exited process is a completed task rather than a
/// failure.
#[tokio::test]
async fn stopped_liveness_projection_carries_the_terminal_instant() {
    let temp = TempDir::new().expect("tempdir");
    let last_observed: chrono::DateTime<Utc> = "2026-08-28T00:00:00Z".parse().expect("fixture instant");
    let entry = projected_liveness_entry(
        &temp,
        record_with_terminal_state("background-worker", Some("session-a"), "running", 0, false, None),
        false,
    )
    .await;

    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped);
    assert_eq!(
        entry.ended_at,
        Some(last_observed),
        "a stopped projection must report the record's last observed instant as its end"
    );
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
        projected.iter().map(|observed| observed.entry.status).collect::<Vec<_>>(),
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

/// A liveness read that fails for any reason other than a missing session must
/// not be reported as an exited process.
///
/// "The process exited" and "the read failed" are different facts. The record
/// keeps its cached status, the caller can see the entry was unobserved with a
/// bounded reason, and one bad entry does not drop the rest of the list.
#[tokio::test]
async fn liveness_read_failure_keeps_the_cached_status_and_is_unobserved() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    write_ledger(
        &state_dir.join("background_subagents.json"),
        vec![record_with_terminal_state(
            "background-worker",
            Some("session-a"),
            "running",
            1,
            true,
            None,
        )],
    );
    let config = auto_restore_config(temp.path().to_path_buf(), true);
    config.exec_sessions.fail_snapshot_reads("exec session store is unavailable");
    let controller = SubagentController::new_with_background_owner(config, Some("session-a"))
        .await
        .expect("controller");

    let observed = controller.background_status_entries_with_liveness().await;
    assert_eq!(observed.len(), 1, "one unobserved record must not abort the list");
    let entry = observed
        .iter()
        .find(|observed| observed.entry.id == "background-worker@session-a")
        .expect("owned background record");

    assert_eq!(
        entry.entry.status,
        BackgroundSubprocessStatus::Running,
        "a failed liveness read must not fabricate a terminal status"
    );
    assert!(entry.entry.ended_at.is_none(), "an unobserved record keeps its active terminal state");
    match &entry.liveness {
        BackgroundLiveness::Unobserved { reason } => {
            assert!(
                reason.contains("exec session store is unavailable"),
                "the reason must name the failed read, got: {reason}"
            );
            assert!(!reason.contains('/'), "the bounded reason must not carry a path, got: {reason}");
        }
        BackgroundLiveness::Observed => panic!("a failed liveness read must be reported as unobserved"),
    }
}

/// A snapshot read is a pure query; only the explicitly named refresh settles
/// or persists a record.
///
/// The record is cached as `Running` and its exec session is gone, so a
/// refresh settles it as `Error`. The pure read must report the cached status
/// and leave the ledger untouched, while the refresh form must produce the
/// settled outcome.
#[tokio::test]
async fn background_snapshot_is_pure_and_refresh_is_explicit() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let ledger_path = state_dir.join("background_subagents.json");
    write_ledger(
        &ledger_path,
        vec![record_with_terminal_state(
            "background-worker",
            Some("session-a"),
            "running",
            1,
            true,
            None,
        )],
    );
    let controller = SubagentController::new_with_background_owner(
        auto_restore_config(temp.path().to_path_buf(), true),
        Some("session-a"),
    )
    .await
    .expect("controller");
    let ledger_before = std::fs::read(&ledger_path).expect("read ledger before snapshot");

    let cached = controller
        .background_snapshot("background-worker@session-a")
        .await
        .expect("cached snapshot");
    assert_eq!(
        cached.entry.status,
        BackgroundSubprocessStatus::Running,
        "the pure snapshot must not settle a record"
    );
    assert_eq!(cached.entry.ended_at, None, "a pure read records no terminal instant");
    assert_eq!(
        std::fs::read(&ledger_path).expect("read ledger after snapshot"),
        ledger_before,
        "the pure snapshot must not persist the ledger"
    );

    let refreshed = controller
        .refresh_then_background_snapshot("background-worker@session-a")
        .await
        .expect("refreshed snapshot");
    assert_eq!(
        refreshed.entry.status,
        BackgroundSubprocessStatus::Error,
        "the refresh form must settle an exited process"
    );
    assert!(refreshed.entry.ended_at.is_some(), "a settled record carries a terminal instant");
}

/// A launch whose exec session never started must settle as an error.
///
/// Refresh treats a `Starting` record as active, and the liveness overlay
/// preserves `Starting` while the launch is desired, so an unsettled failure
/// that no restart will retry would be reported as a running process forever.
/// The default toggle would then route a graceful stop to a process that never
/// started. The spawn failure is deterministic: the demo agent's launch command
/// is a workspace script, and spawning a missing program fails before any
/// process exists.
#[tokio::test]
async fn failed_background_spawn_settles_the_record_as_an_error() {
    let temp = TempDir::new().expect("tempdir");
    write_test_background_subagent(temp.path());
    std::fs::remove_file(temp.path().join("scripts").join("demo-background-subagent.sh"))
        .expect("remove the launch script so the spawn fails");
    let mut cfg = DiapasonConfig::default();
    cfg.subagents.background.enabled = true;
    cfg.subagents.background.auto_restore = false;
    let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf(), cfg))
        .await
        .expect("controller");
    controller.set_turn_delegation_hints_from_input("delegate this task").await;

    let error = controller
        .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
            agent_type: Some("background-demo".to_string()),
            ..SpawnBackgroundSubprocessRequest::default()
        })
        .await
        .expect_err("a missing launch script must fail the spawn");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("Failed to spawn background subprocess"),
        "the caller keeps the contextual spawn failure, got: {rendered}"
    );

    let entry = controller
        .background_status_entries()
        .await
        .into_iter()
        .find(|entry| entry.agent_name == "background-demo")
        .expect("the failed launch leaves a record");
    assert_eq!(
        entry.status,
        BackgroundSubprocessStatus::Error,
        "a failed launch with no restart left must not stay Starting"
    );
    let message = entry.error.expect("a settled failure must be explained");
    assert!(message.contains("background-demo"), "the failure must name the agent, got: {message}");
    assert!(
        message.contains("failed to spawn pipe session") || message.contains("No such file"),
        "the failure must include the spawn error, got: {message}"
    );
    assert!(entry.ended_at.is_some(), "a settled failure carries a terminal instant");
    assert!(entry.desired_enabled, "settling a failed launch must not clear the desired state");
}

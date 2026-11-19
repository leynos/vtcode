//! Background output-tail queries and owner round-trips.
//!
//! Reading a tail is a pure query over cached state: it must not settle a
//! record, publish a lifecycle update, or rewrite the ledger. The tail bound
//! returns exactly the last requested lines, and an owner id survives
//! persistence into the public status projection.
use super::*;
use crate::subagents::background::background_state_path_for_tests;
use proptest::prelude::*;

/// Reading a background output tail is a query. It must not refresh process
/// state, republish lifecycle updates, or rewrite the persisted ledger, even
/// when the recorded exec session is gone and a refresh would settle the
/// record.
#[tokio::test]
async fn background_output_tail_reads_without_refreshing_background_state() {
    let temp = TempDir::new().expect("tempdir");
    let state_path = background_state_path_for_tests(temp.path());
    std::fs::create_dir_all(state_path.parent().expect("state dir")).expect("state dir");
    let record: PersistedBackgroundRecord = serde_json::from_value(serde_json::json!({
        "id": "background-worker",
        "agent_name": "worker",
        "display_label": "worker",
        "description": "worker",
        "source": "builtin",
        "color": null,
        "owner_session_id": "session-a",
        "session_id": "session-background-worker",
        "exec_session_id": "exec-background-worker",
        "desired_enabled": false,
        "status": "running",
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": "2026-08-28T00:00:00Z",
        "started_at": "2026-08-28T00:00:00Z",
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
    .expect("persisted background record");
    std::fs::write(
        &state_path,
        serde_json::to_string(&PersistedBackgroundState { records: vec![record] }).expect("serialize state"),
    )
    .expect("write state");
    let persisted_before = std::fs::read(&state_path).expect("read persisted state");

    let controller = SubagentController::new_with_background_owner(
        test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()),
        Some("session-a"),
    )
    .await
    .expect("controller");
    let task_id = "background-worker@session-a";
    let before = controller
        .background_status_entries()
        .await
        .into_iter()
        .find(|entry| entry.id == task_id)
        .expect("owned background record");

    let output = controller.background_output_tail(task_id, 10).await.expect("output tail");

    assert_eq!(output, "");
    let after = controller
        .background_status_entries()
        .await
        .into_iter()
        .find(|entry| entry.id == task_id)
        .expect("owned background record");
    assert_eq!(
        after.status,
        BackgroundSubprocessStatus::Running,
        "the output query must not settle a record whose exec session is gone"
    );
    assert_eq!(after.updated_at, before.updated_at, "the output query must not touch record timestamps");
    assert_eq!(
        std::fs::read(&state_path).expect("reread persisted state"),
        persisted_before,
        "the output query must not rewrite persisted background state"
    );
}

proptest! {
    /// An owner id round-trips through persistence into the public status
    /// entry, including a legacy ownerless record.
    #[test]
    fn owner_round_trip_preserves_the_owner(
        owner_session_id in prop::option::of("[a-z0-9-]{1,32}"),
    ) {
        let record = persisted_background_record_for_owner("background-worker", owner_session_id.as_deref());
        let runtime = BackgroundRecord::from_persisted(record);

        prop_assert_eq!(runtime.build_status_entry().owner_session_id, owner_session_id);
    }

    /// `extract_tail_lines` returns exactly the last `min(line_count,
    /// requested)` lines. A bound-only assertion would also pass for empty or
    /// first-N output, so compare against the expected tail.
    #[test]
    fn output_tail_returns_exactly_the_last_requested_lines(
        line_count in 0usize..=20_000,
        requested in 0usize..=20_000,
    ) {
        let content = (0..line_count).map(|line| line.to_string()).collect::<Vec<_>>().join("\n");
        let tail = extract_tail_lines(&content, requested);
        let expected_count = line_count.min(requested);

        prop_assert_eq!(tail.lines().count(), expected_count);

        let expected = (line_count - expected_count..line_count)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        prop_assert_eq!(tail, expected);
    }
}

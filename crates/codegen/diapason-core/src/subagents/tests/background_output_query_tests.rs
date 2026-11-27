//! Background output-tail queries and owner round-trips.
//!
//! Reading a tail is a pure query over cached state: it must not settle a
//! record, publish a lifecycle update, or rewrite the ledger. The tail bound
//! returns exactly the last requested lines, and an owner id survives
//! persistence into the public status projection.
use super::*;
use crate::subagents::background::background_state_path_for_tests;
use proptest::prelude::*;
use std::sync::Mutex;

/// Counts the background lifecycle updates a controller publishes.
///
/// An output read is a query, so the count must stay at zero across it. A
/// status-only assertion cannot show that: the record could settle and be
/// republished while the projected status and timestamps still matched the
/// values read before, if a refresh wrote them back unchanged.
#[derive(Default)]
struct BackgroundPublicationCounter {
    published: Mutex<Vec<BackgroundSubprocessEntry>>,
}

impl SubagentProgressObserver for BackgroundPublicationCounter {
    fn subagent_status_changed(&self, _entry: SubagentStatusEntry) {}

    fn background_status_changed(&self, entry: BackgroundSubprocessEntry) {
        self.background_status_published(entry, 0);
    }

    fn background_status_published(&self, entry: BackgroundSubprocessEntry, _lifecycle_revision: u64) {
        self.published.lock().expect("background lock").push(entry);
    }
}

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
        // The observer registered below is keyed by this session, so a query
        // that did publish would reach it. An empty parent would silence the
        // routing and make the no-publication assertion vacuous.
        "parent_session_id": "session-a",
        "session_id": "session-background-worker",
        "exec_session_id": "exec-session-background-worker-1-1",
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
    // The observer is registered before the read, so any lifecycle update the
    // read emitted is captured rather than lost to registration order.
    let observer = Arc::new(BackgroundPublicationCounter::default());
    let _registration = controller.register_progress_observer("session-a", observer.clone());
    let before = controller
        .background_status_entries(TaskOwner::Workspace)
        .await
        .into_iter()
        .find(|entry| entry.id == task_id)
        .expect("owned background record");
    // Count only the publications the output read causes: earlier projections
    // may legitimately publish, and the assertion must not depend on which of
    // them ran before the observer was registered.
    let published_before = observer.published.lock().expect("background lock").len();

    let output = controller
        .background_output_tail(TaskOwner::Workspace, task_id, 10)
        .await
        .expect("output tail");

    assert_eq!(output, "");
    assert_eq!(
        observer.published.lock().expect("background lock").len(),
        published_before,
        "the output query must not publish a background lifecycle update"
    );
    let after = controller
        .background_status_entries(TaskOwner::Workspace)
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

/// A forged identity pair is refused before its live session is read.
///
/// A row restored from the ledger carries both identity fields as stored data,
/// so it can name another task's live exec session while claiming a different
/// session. The pairing was only checked on the archive fallback, so a row
/// whose forged exec id pointed at a live session with output was served as
/// that task's output. The pairing must be checked before either storage layer
/// is read.
#[tokio::test]
#[cfg(unix)]
async fn background_output_tail_refuses_a_forged_pair_before_the_live_read() {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().to_path_buf();
    let vt_cfg = DiapasonConfig::default();
    let pty_sessions = PtySessionManager::new(root.clone(), vt_cfg.pty.clone());
    let exec_sessions = ExecSessionManager::new(root.clone(), pty_sessions.clone());
    let mut config = test_controller_config(root.clone(), vt_cfg);
    config.exec_sessions = exec_sessions.clone();
    config.pty_manager = pty_sessions.manager().clone();

    // A live session that belongs to `victim-child` and has output to leak.
    let victim_session_id = controller_exec_session_id("victim-child", std::process::id(), 0);
    exec_sessions
        .create_pipe_session(
            victim_session_id.clone().into(),
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf 'victim output'".to_string(),
            ],
            root,
            hashbrown::HashMap::new(),
        )
        .await
        .expect("live victim session");
    let leaked = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match exec_sessions.read_session_output(victim_session_id.as_str(), false).await {
                Ok(Some(output)) => return output,
                Ok(None) => {}
                Err(error) => panic!("the fixture's live read failed: {error:#}"),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the victim session must produce output before the forged read");
    assert_eq!(leaked, "victim output", "the fixture must have live output to leak");

    let controller = SubagentController::new(config).await.expect("controller");
    let forged = BackgroundRecord::from_persisted(
        serde_json::from_value(serde_json::json!({
            "id": "background-worker",
            "agent_name": "worker",
            "display_label": "worker",
            "description": "worker",
            "source": "builtin",
            "color": null,
            "owner_session_id": "session-a",
            "parent_session_id": "session-a",
            "session_id": "other-child",
            "exec_session_id": victim_session_id.as_str(),
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
        .expect("forged persisted record"),
    );
    let task_id = forged.id.clone();
    controller
        .state
        .write()
        .await
        .background_children
        .insert(task_id.clone(), forged);

    let error = controller
        .background_output_tail(TaskOwner::Workspace, &task_id, 10)
        .await
        .expect_err("a forged pair must not be served another task's live output");
    assert!(
        format!("{error:#}").contains("does not embed"),
        "the refusal must name the missing pairing: {error:#}"
    );

    exec_sessions
        .close_session(victim_session_id.as_str())
        .await
        .expect("close the victim session");
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

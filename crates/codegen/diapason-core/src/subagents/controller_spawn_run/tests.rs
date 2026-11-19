use super::super::background::load_background_state;
use super::*;
use std::time::Duration;

use crate::config::constants::models;
use crate::config::constants::tools;
use crate::tools::registry::PtySessionManager;
use diapason_config::SubagentSource;
use diapason_config::core::permissions::{AgentPermissionsConfig, PermissionDefault};

/// Builds a controller configuration for a temporary workspace.
///
/// The fixture mirrors the one the subagent module's own tests use, kept local
/// because this module's tests cannot reach the private helper there.
fn test_config(workspace_root: PathBuf) -> SubagentControllerConfig {
    let vt_cfg = DiapasonConfig::default();
    let pty_sessions = PtySessionManager::new(workspace_root.clone(), vt_cfg.pty.clone());
    let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
    SubagentControllerConfig {
        workspace_root,
        parent_session_id: "parent-session".to_string(),
        parent_model: models::openai::GPT_5_6_SOL.to_string(),
        parent_provider: "openai".to_string(),
        parent_reasoning_effort: ReasoningEffortLevel::Medium,
        api_key: "test-key".to_string(),
        vt_cfg,
        openai_chatgpt_auth: None,
        depth: 0,
        workspace_gated: false,
        exec_sessions,
        output_reader: None,
        pty_manager: pty_sessions.manager().clone(),
        managed_background_runtime: false,
        lock_scope: None,
    }
}

/// Builds a minimal read-only subagent spec for a tracked child record.
fn test_spec() -> SubagentSpec {
    let mut permissions = AgentPermissionsConfig::new(PermissionDefault::Deny);
    permissions.allow = vec![tools::READ_FILE.to_string()];
    SubagentSpec {
        name: "worker".to_string(),
        description: "test worker".to_string(),
        prompt: String::new(),
        tools: Some(vec![tools::READ_FILE.to_string()]),
        disallowed_tools: Vec::new(),
        model: None,
        colour: None,
        reasoning_effort: None,
        permissions,
        skills: Vec::new(),
        mcp_servers: Vec::new(),
        hooks: None,
        background: false,
        mode: diapason_config::AgentMode::Subagent,
        max_turns: None,
        nickname_candidates: Vec::new(),
        initial_prompt: None,
        memory: None,
        isolation: None,
        aliases: Vec::new(),
        source: SubagentSource::Builtin,
        file_path: None,
        warnings: Vec::new(),
        tool_policy_overrides: Default::default(),
    }
}

/// Builds a tracked child record in `status`, with no live handle.
fn child_record(id: &str, status: SubagentStatus) -> ChildRecord {
    let spec = test_spec();
    ChildRecord {
        id: id.to_string(),
        session_id: format!("parent-session-{id}"),
        parent_thread_id: "parent-session".to_string(),
        display_label: subagent_display_label(&spec),
        spec,
        status,
        background: false,
        depth: 1,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        completed_at: status.is_terminal().then_some(Utc::now()),
        summary: None,
        error: None,
        archive_metadata: None,
        archive_path: None,
        transcript_path: None,
        effective_config: Some(DiapasonConfig::default()),
        stored_messages: Vec::new(),
        last_prompt: Some(format!("prompt-{id}")),
        queued_prompts: VecDeque::new(),
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        thread_handle: None,
        handle: None,
        notify: Arc::new(Notify::new()),
        worktree_path: None,
        child_controller: None,
    }
}

/// Builds one canonical ledger record for `agent` under `owner`.
fn persisted_record(id: &str, agent: &str, owner: &str) -> PersistedBackgroundRecord {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "owner_session_id": owner,
        "agent_name": agent,
        "display_label": agent,
        "description": "test record",
        "source": "builtin",
        "color": null,
        "session_id": format!("session-{id}"),
        "exec_session_id": format!("exec-{id}"),
        "desired_enabled": true,
        "status": "running",
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": "2026-08-28T00:00:00Z",
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

/// Inserts a persisted record into a controller's in-memory ledger view.
async fn insert_background(controller: &SubagentController, record: PersistedBackgroundRecord) {
    let id = record.id.clone();
    controller
        .state
        .write()
        .await
        .background_children
        .insert(id, BackgroundRecord::from_persisted(record));
}

/// Returns the ids the shared ledger currently holds, in file order.
async fn ledger_ids(workspace_root: &std::path::Path) -> Vec<String> {
    load_background_state(workspace_root)
        .await
        .expect("load background ledger")
        .records
        .into_iter()
        .map(|record| record.id)
        .collect()
}

/// A registered waiter must retain a wakeup delivered before its first poll.
#[tokio::test]
async fn registered_waiter_observes_notify_waiters_before_first_wait_poll() {
    let notify = Arc::new(Notify::new());
    let notifies = [Arc::clone(&notify)];
    let waiters = register_notify_waiters(&notifies);

    // notify_waiters() is lossy for futures that have not registered.
    // Deliver it after enable() but before select_all first polls a waiter.
    notify.notify_waiters();

    let outcome = tokio::time::timeout(Duration::from_millis(50), select_all(waiters)).await;
    assert!(outcome.is_ok(), "registered waiter should retain the wakeup");
}

/// An explicitly closed child must refuse input even when it completed.
///
/// `close_single` keeps a settled `Completed` outcome visible so a transport
/// does not project a successful task as failed. `send_input` therefore cannot
/// use the status alone to refuse a closed task, or the user could restart work
/// they had explicitly stopped. The closure is tracked separately, survives the
/// terminal outcome, and is cleared only by reopening.
#[tokio::test]
async fn send_input_rejects_a_completed_child_that_was_closed() {
    let workspace = tempfile::tempdir().expect("workspace");
    let controller =
        SubagentController::new_with_background_owner(test_config(workspace.path().to_path_buf()), Some("session-a"))
            .await
            .expect("controller");
    let id = controller
        .insert_child_with_candidate_id("agent-worker", child_record("agent-worker", SubagentStatus::Completed))
        .await
        .expect("insert child");

    let closed = controller.close(&id).await.expect("close child");
    assert_eq!(closed.status, SubagentStatus::Completed, "closing a settled child must preserve its outcome");
    assert!(is_child_closed(&controller.state, &id), "the explicit closure must be remembered");

    let error = match controller
        .send_input(SendInputRequest {
            target: id.clone(),
            message: Some("do more work".to_string()),
            items: Vec::new(),
            interrupt: false,
        })
        .await
    {
        Ok(_) => panic!("input must be rejected while the child is closed"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("closed"), "the rejection must name the closure: {error:#}");
    let status = controller.status_for(&id).await.expect("status");
    assert_eq!(status.status, SubagentStatus::Completed, "no new run may start");
    assert!(
        controller
            .state
            .read()
            .await
            .children
            .get(&id)
            .is_some_and(|record| record.queued_prompts.is_empty()),
        "the rejected input must not be queued for a later run"
    );

    assert!(controller.reopen_single(&id).await.expect("reopen"));
    assert!(!is_child_closed(&controller.state, &id), "reopening must clear the closure");
}

/// A save must not delete a record another controller of the same owner wrote.
///
/// Two live controllers can share one owner session. Each loaded the ledger at
/// construction, so a record one persists afterwards is absent from the other's
/// snapshot. Saving the stale snapshot must keep that record, while an owner
/// record this controller did commit and then dropped is still removed.
#[tokio::test]
async fn owner_save_keeps_a_record_another_controller_persisted() {
    let workspace = tempfile::tempdir().expect("workspace");
    let root = workspace.path().to_path_buf();
    let controller_a = SubagentController::new_with_background_owner(test_config(root.clone()), Some("session-a"))
        .await
        .expect("controller a");
    let worker = persisted_record("background-worker@session-a", "worker", "session-a");
    insert_background(&controller_a, worker.clone()).await;
    controller_a.save_background_state().await.expect("save a");
    assert_eq!(ledger_ids(&root).await, vec![worker.id.clone()]);

    // The second controller of the same owner writes after the first loaded.
    let controller_b = SubagentController::new_with_background_owner(test_config(root.clone()), Some("session-a"))
        .await
        .expect("controller b");
    let reviewer = persisted_record("background-reviewer@session-a", "reviewer", "session-a");
    insert_background(&controller_b, reviewer.clone()).await;
    controller_b.save_background_state().await.expect("save b");
    let mut both = ledger_ids(&root).await;
    both.sort();
    let mut expected = vec![worker.id.clone(), reviewer.id.clone()];
    expected.sort();
    assert_eq!(both, expected);

    // A now drops its own record and saves the empty snapshot. The id it
    // committed is removed; B's record, which A never committed, survives.
    controller_a.state.write().await.background_children.clear();
    controller_a.save_background_state().await.expect("save a again");
    assert_eq!(ledger_ids(&root).await, vec![reviewer.id.clone()]);
}

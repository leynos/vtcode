//! `wait` returns the first terminal child.
use super::*;

#[tokio::test]
async fn wait_returns_first_terminal_child() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");
    let spec = diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "default")
        .expect("default");

    {
        let mut state = controller.state.write().await;
        for id in ["first", "second"] {
            state.children.insert(
                id.to_string(),
                ChildRecord {
                    id: id.to_string(),
                    session_id: format!("session-{id}"),
                    parent_thread_id: "parent-session".to_string(),
                    spec: spec.clone(),
                    display_label: subagent_display_label(&spec),
                    status: SubagentStatus::Running,
                    background: false,
                    depth: 1,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    completed_at: None,
                    summary: None,
                    error: None,
                    archive_metadata: None,
                    archive_path: None,
                    transcript_path: None,
                    effective_config: None,
                    stored_messages: Vec::new(),
                    last_prompt: None,
                    queued_prompts: VecDeque::new(),
                    max_turns: None,
                    model_override: None,
                    reasoning_override: None,
                    thread_handle: None,
                    handle: None,
                    notify: Arc::new(Notify::new()),
                    worktree_path: None,
                    child_controller: None,
                },
            );
        }
    }

    // Drive `wait` through its first poll inside the waiter and signal only
    // after that poll returns `Pending`. The first poll is the readiness point:
    // it only suspends once the child notifications are registered, so
    // completing `second` afterwards must wake the waiter through them. A
    // regression to a delay, or to a scheduling order that lets `wait` take its
    // initial-scan fast path, no longer produces the `Pending` first poll this
    // test requires.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let waiter = {
        let controller = controller.clone();
        tokio::spawn(async move {
            let targets = ["first".to_string(), "second".to_string()];
            let wait = controller.wait(&targets, Some(500));
            let mut wait = std::pin::pin!(wait);
            let first = futures::poll!(wait.as_mut());
            let _ = ready_tx.send(());
            assert!(first.is_pending(), "wait must park until a wakeup, not take the fast path");
            wait.await
        })
    };
    ready_rx.await.expect("the waiter reached its first suspension");

    {
        let mut state = controller.state.write().await;
        let record = state.children.get_mut("second").expect("second child");
        record.status = SubagentStatus::Completed;
        record.summary = Some("done".to_string());
        record.completed_at = Some(Utc::now());
        record.updated_at = Utc::now();
        record.notify.notify_waiters();
    }

    // Joining the waiter also fails the test if it only woke on the timeout:
    // the completion must reach it through the registered notification.
    let result = waiter
        .await
        .expect("waiter task")
        .expect("wait result")
        .expect("the notification must wake the parked waiter");
    assert_eq!(result.id, "second");
    assert_eq!(result.status, SubagentStatus::Completed);
}

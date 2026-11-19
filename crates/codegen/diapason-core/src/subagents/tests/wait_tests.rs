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

    let controller_clone = controller.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut state = controller_clone.state.write().await;
        let record = state.children.get_mut("second").expect("second child");
        record.status = SubagentStatus::Completed;
        record.summary = Some("done".to_string());
        record.completed_at = Some(Utc::now());
        record.updated_at = Utc::now();
        record.notify.notify_waiters();
    });

    let result = controller
        .wait(&["first".to_string(), "second".to_string()], Some(500))
        .await
        .expect("wait result")
        .expect("terminal child");
    assert_eq!(result.id, "second");
    assert_eq!(result.status, SubagentStatus::Completed);
}

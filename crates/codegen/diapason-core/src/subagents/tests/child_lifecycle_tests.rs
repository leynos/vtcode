//! Closing a subagent must stop its task without rewriting a settled outcome.
use super::*;

/// The built-in explorer spec a child record needs.
fn explorer_spec() -> SubagentSpec {
    diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "explorer")
        .expect("explorer spec")
}

/// A terminal record can still own a running task.
///
/// The child loop writes its terminal status before it finishes: it still has
/// to run the stop hook and its own completion bookkeeping, and the stored
/// handle is only cleared once the loop goes idle. Closing the child during
/// that window must still abort the task, because the loop would otherwise go
/// on to close its nested subagents and reconcile its worktree after being
/// cancelled. The settled status and completion instant are preserved.
#[tokio::test]
async fn closing_a_terminal_child_still_aborts_its_task() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");
    let spec = explorer_spec();

    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (alive_tx, alive_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        // Dropping the sender is how the test observes that the task ended.
        let _alive = alive_tx;
        let _ = started_tx.send(());
        std::future::pending::<()>().await;
    });
    started_rx.await.expect("the child task started");

    let mut record =
        test_child_record("child", "child-session", "parent-session", &spec, SubagentStatus::Completed, 1, None);
    let completed_at = record.completed_at;
    record.handle = Some(handle);
    {
        let mut state = controller.state.write().await;
        state.children.insert("child".to_string(), record);
    }

    let closed = controller.close("child").await.expect("close the child");

    // The abort is observed first: the settled outcome must be untouched, and
    // the task must be gone even though the record already looked terminal.
    let aborted = tokio::time::timeout(Duration::from_secs(5), alive_rx).await;
    assert!(
        matches!(aborted, Ok(Err(_))),
        "closing a terminal child must abort its stored task, got {aborted:?}"
    );
    assert_eq!(closed.status, SubagentStatus::Completed, "closing must not rewrite a settled outcome");
    assert_eq!(closed.completed_at, completed_at, "closing must keep the completion instant the loop recorded");
}

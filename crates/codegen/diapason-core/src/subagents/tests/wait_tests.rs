//! `wait` returns the first terminal child.
use super::super::controller_spawn_run::wait_checkpoints;
use super::*;
use crate::core::agent::task::TaskOutcome;

// The clock is paused so the 500 ms deadline is virtual. A regression that
// stops waking the parked waiter through its notification then auto-advances to
// the deadline and returns `None` immediately, instead of racing a real 500 ms
// delay on a loaded scheduler.
#[tokio::test(start_paused = true)]
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
                test_child_record(
                    id,
                    &format!("session-{id}"),
                    "parent-session",
                    &spec,
                    SubagentStatus::Running,
                    1,
                    None,
                ),
            );
        }
    }

    // Park the waiter once it has created its notification futures and has
    // rechecked both target statuses. Readiness comes from that checkpoint
    // itself rather than from a delay or a first-poll assumption, so completing
    // `second` afterwards can no longer be observed by the recheck: it must
    // wake the waiter through its registered notifications.
    let (entered, release) = wait_checkpoints::arm(&controller.state);
    let waiter = {
        let controller = controller.clone();
        tokio::spawn(async move {
            let targets = ["first".to_string(), "second".to_string()];
            let wait = controller.wait(TaskOwner::Workspace, &targets, Some(500));
            let mut wait = std::pin::pin!(wait);
            // The checkpoint this test arms is reached inside this first poll,
            // so the assertion also confirms the waiter did not finish on the
            // initial scan.
            let first = futures::poll!(wait.as_mut());
            assert!(first.is_pending(), "wait must park until a wakeup, not take the fast path");
            wait.await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the waiter must reach the checkpoint after registering its notifications");

    {
        let mut state = controller.state.write().await;
        let record = state.children.get_mut("second").expect("second child");
        // The child completes through the same path a finished run takes. That
        // path stores the result, stamps `completed_at` and notifies the
        // waiters, so the test proves the waiter wakes on a real completion
        // rather than on fields this test wrote by hand.
        let result = ChildRunResult {
            messages: Vec::new(),
            summary: "done".to_string(),
            outcome: TaskOutcome::Success,
            transcript_path: None,
        };
        assert!(!record.apply_result(Ok(result)), "a single run leaves no queued work behind");
    }
    release.notify_one();

    // Joining the waiter also fails the test if it only woke on the timeout:
    // the completion must reach it through the registered notification.
    let result = waiter
        .await
        .expect("waiter task")
        .expect("the wait must not fail")
        .expect("the notification must wake the parked waiter");
    assert_eq!(result.id, "second");
    assert_eq!(result.status, SubagentStatus::Completed);
}

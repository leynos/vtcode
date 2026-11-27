//! Verifies that shared subagent controllers isolate parent context and child visibility per invocation.
use super::*;

use super::*;

async fn shared_context_controller() -> (TempDir, SubagentController) {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");
    let spec = read_only_test_spec("shared-context-child");
    {
        let mut state = controller.state.write().await;
        state.children.insert(
            "child-alpha".to_string(),
            test_child_record(
                "child-alpha",
                "child-alpha-session",
                "session-alpha",
                &spec,
                SubagentStatus::Running,
                1,
                None,
            ),
        );
        state.children.insert(
            "child-beta".to_string(),
            test_child_record(
                "child-beta",
                "child-beta-session",
                "session-beta",
                &spec,
                SubagentStatus::Running,
                1,
                None,
            ),
        );
    }
    (temp, controller)
}

async fn verify_session_scoped_visibility(
    controller: SubagentController,
    rendezvous: Arc<tokio::sync::Barrier>,
    owner: &'static str,
    expected_child: &'static str,
) {
    let message = Message {
        role: MessageRole::User,
        content: MessageContent::text(format!("history-{owner}")),
        ..Message::default()
    };
    let context = SubagentInvocationContext::new(owner.to_string(), vec![message], format!("delegate task {owner}"));
    with_subagent_invocation_context(context, async move {
        rendezvous.wait().await;
        tokio::task::yield_now().await;
        let mut items = Vec::new();
        let mut model = None;
        let prepared = controller
            .prepare_delegation_context(None, &mut items, &mut model, "spawn_agent")
            .await
            .expect("prepare session context");
        assert_eq!(prepared.parent_session_id, owner);
        assert_eq!(prepared.parent_messages[0].content.as_text(), format!("history-{owner}"));
        assert!(prepared.explicit_request);
        let visible_children = controller.status_entries().await;
        assert_eq!(visible_children.len(), 1);
        assert_eq!(visible_children[0].id, expected_child);
        let other_child = if expected_child == "child-alpha" {
            "child-beta"
        } else {
            "child-alpha"
        };
        assert!(controller.status_for(TaskOwner::Session(owner), other_child).await.is_err());
        assert!(controller.close(TaskOwner::Session(owner), other_child).await.is_err());
        assert!(
            controller
                .wait(TaskOwner::Session(owner), &[other_child.to_string()], Some(0))
                .await
                .is_err()
        );
    })
    .await;
}

#[tokio::test]
async fn shared_controller_scopes_parent_context_and_child_visibility_per_invocation() {
    let (_temp, controller) = shared_context_controller().await;
    let rendezvous = Arc::new(tokio::sync::Barrier::new(2));
    tokio::join!(
        verify_session_scoped_visibility(controller.clone(), rendezvous.clone(), "session-alpha", "child-alpha"),
        verify_session_scoped_visibility(controller.clone(), rendezvous, "session-beta", "child-beta"),
    );
    assert_eq!(
        controller
            .status_for(TaskOwner::Workspace, "child-alpha")
            .await
            .expect("alpha status")
            .status,
        SubagentStatus::Running
    );
    assert_eq!(
        controller
            .status_for(TaskOwner::Workspace, "child-beta")
            .await
            .expect("beta status")
            .status,
        SubagentStatus::Running
    );
}

/// A session-scoped command must judge the authority it was given, not the
/// ambient invocation context.
///
/// These calls run outside any `with_subagent_invocation_context` scope, so
/// the old ambient lookup would have treated the foreign child as addressable.
/// The explicit owner must reject it with the same not-found error every other
/// command produces, which also keeps the task's existence unobservable.
#[tokio::test]
async fn session_scoped_commands_reject_a_foreign_child_without_an_invocation_context() {
    let (_temp, controller) = shared_context_controller().await;
    let foreign_owner = TaskOwner::Session("other-session");
    let foreign_child = "child-alpha";

    let resume_error = controller
        .resume(foreign_owner, foreign_child)
        .await
        .expect_err("a session must not resume another session's child");
    assert!(
        resume_error.to_string().contains("Unknown subagent id child-alpha"),
        "resume must reject the foreign child as unknown: {resume_error}"
    );

    let wait_error = controller
        .wait(foreign_owner, &[foreign_child.to_string()], Some(0))
        .await
        .expect_err("a session must not wait on another session's child");
    assert!(
        wait_error.to_string().contains("Unknown subagent id child-alpha"),
        "wait must reject the foreign child as unknown: {wait_error}"
    );

    let send_input_error = controller
        .send_input(
            foreign_owner,
            SendInputRequest {
                target: foreign_child.to_string(),
                message: Some("do more work".to_string()),
                items: Vec::new(),
                interrupt: false,
            },
        )
        .await
        .expect_err("a session must not send input to another session's child");
    assert!(
        send_input_error.to_string().contains("Unknown subagent id child-alpha"),
        "send_input must reject the foreign child as unknown: {send_input_error}"
    );
}

/// The owning session still addresses its child with no invocation context.
///
/// The authority is the explicit owner, so dropping the ambient fallback must
/// not narrow a session that genuinely owns the task.
#[tokio::test]
async fn session_scoped_commands_accept_a_child_the_session_owns() {
    let (_temp, controller) = shared_context_controller().await;
    let owner = TaskOwner::Session("session-alpha");

    assert!(
        controller.wait(owner, &["child-alpha".to_string()], Some(0)).await.is_ok(),
        "the owning session must be able to wait on its own child"
    );
    let resumed = controller
        .resume(owner, "child-alpha")
        .await
        .expect("the owning session must resume its own child");
    assert_eq!(resumed.id, "child-alpha");

    let sent = controller
        .send_input(
            owner,
            SendInputRequest {
                target: "child-alpha".to_string(),
                message: Some("continue".to_string()),
                items: Vec::new(),
                interrupt: false,
            },
        )
        .await
        .expect("the owning session must send input to its own child");
    assert_eq!(sent.id, "child-alpha");
}

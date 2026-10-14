//! Verifies that shared subagent controllers isolate parent context and child visibility per invocation.
use super::*;

use super::*;

async fn shared_context_controller() -> (TempDir, SubagentController) {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default()))
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
        assert!(controller.status_for(other_child).await.is_err());
        assert!(controller.close(other_child).await.is_err());
        assert!(controller.wait(&[other_child.to_string()], Some(0)).await.is_err());
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
    assert_eq!(controller.status_for("child-alpha").await.expect("alpha status").status, SubagentStatus::Running);
    assert_eq!(controller.status_for("child-beta").await.expect("beta status").status, SubagentStatus::Running);
}

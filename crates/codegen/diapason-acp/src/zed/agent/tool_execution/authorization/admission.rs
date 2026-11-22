//! Admission of subagent tool calls against the session's own controller.
//!
//! The advertised tool list can come from the workspace registry, which knows
//! the workspace controller, while a call must execute through the session's
//! controller. These checks reject a call the session cannot own.
//!
//! Admission runs twice on purpose. The early check runs before any hook,
//! prompt or resolution and fails closed only when the session has no
//! controller: the model's raw arguments may name an obsolete target, and a
//! `PermissionRequest` hook may replace them before dispatch. The second check
//! runs after the permission stages and judges per-target ownership on the
//! arguments that will actually execute; without it a rewrite could name a
//! target this session does not own and only core would later refuse the call.

use diapason_core::config::constants::tools;
use diapason_core::subagents::TaskOwner;
use serde_json::Value;
use std::sync::Arc;

/// Rejects a subagent tool call when the session has no controller.
///
/// This is the early, pre-hook admission check. It deliberately does not judge
/// the targets the model named: a `PermissionRequest` hook may replace those
/// arguments before dispatch, so ownership is judged only on the arguments that
/// will actually execute. A non-subagent tool is never tied to a controller.
pub(super) fn session_subagent_controller_error(
    tool_name: &str,
    controller: Option<&Arc<diapason_core::subagents::SubagentController>>,
) -> Option<String> {
    if !diapason_core::subagents::is_subagent_tool(tool_name) || controller.is_some() {
        return None;
    }
    // The session controller's absence is reported with the core registry's
    // established wording, so the client and model see the same fail-closed
    // reason on both enforcement paths.
    Some(format!(
        "Subagent controller is not active in this session: tool '{tool_name}' has no session-owned controller"
    ))
}

/// Rejects a subagent tool call this session cannot own.
///
/// The ACP model advertises tools from the workspace registry, which knows the
/// workspace controller, while a call must execute through the session's own
/// controller. Without this admission step a prompt-driven `agent` call could
/// address a target that another session owns, or run through the workspace's
/// ownerless controller on a session that has no controller of its own. A
/// target-less call (`spawn`) needs only the session controller; a call that
/// names `id`/`ids` additionally needs every named target among the records
/// this controller tree holds.
///
/// This is the post-rewrite check. The caller runs it on the arguments a
/// permission hook installed, so a rewrite cannot smuggle in a foreign target;
/// [`session_subagent_controller_error`] is the early check that runs before a
/// hook can replace the raw arguments.
pub(super) async fn session_subagent_admission_error(
    tool_name: &str,
    controller: Option<&Arc<diapason_core::subagents::SubagentController>>,
    arguments: &str,
) -> Option<String> {
    if !diapason_core::subagents::is_subagent_tool(tool_name) {
        return None;
    }
    let Some(controller) = controller else {
        return session_subagent_controller_error(tool_name, None);
    };
    for target in subagent_call_targets(tool_name, arguments) {
        if !session_owns_subagent(controller, target.as_str()).await {
            return Some(format!("Tool '{tool_name}' cannot operate on '{target}': it is not owned by this session"));
        }
    }
    None
}

/// Returns the subagent ids a call names, mirroring the core executor's
/// action inference for the unified `agent` tool.
fn subagent_call_targets(tool_name: &str, arguments: &str) -> Vec<String> {
    let Ok(args) = serde_json::from_str::<Value>(arguments) else {
        // The core executor reports the malformed arguments; there is no
        // target to admit or reject here.
        return Vec::new();
    };
    // The core executor reads the same call through the same parser, so an
    // admission decision cannot be made about a different task than the one
    // the executor then addresses.
    diapason_core::subagents::agent_call_targets(tool_name, &args)
}

/// Whether `target` belongs to this controller tree.
///
/// A session controller holds only the records it owns, and nested delegation
/// puts grandchildren in descendant controllers, so the whole tree is
/// consulted. Reads are pure: `status_entries` and `background_status_entries`
/// never refresh, respawn, persist or publish.
async fn session_owns_subagent(controller: &Arc<diapason_core::subagents::SubagentController>, target: &str) -> bool {
    let mut controllers = vec![Arc::clone(controller)];
    controllers.extend(controller.descendant_controllers().await);
    for controller in controllers {
        if controller.status_entries().await.iter().any(|entry| entry.id == target) {
            return true;
        }
        if controller
            .background_status_entries(TaskOwner::Workspace)
            .await
            .iter()
            .any(|entry| entry.id == target)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The workspace registry may advertise `agent` while the prompting
    /// session has no controller. Execution must reject the call instead of
    /// running it against the shared, ownerless controller.
    #[tokio::test]
    async fn subagent_tools_are_rejected_without_a_session_controller() {
        for tool in [tools::AGENT, tools::SEND_INPUT, tools::WAIT_AGENT] {
            let early = session_subagent_controller_error(tool, None)
                .expect("the early check must fail closed without a session controller");
            assert!(
                early.contains("Subagent controller is not active in this session"),
                "the early rejection must carry the established fail-closed reason: {early}"
            );
            let rejection = session_subagent_admission_error(tool, None, "{}")
                .await
                .expect("a subagent tool must not run without a session controller");
            assert!(
                rejection.contains("Subagent controller is not active in this session"),
                "the rejection must carry the established fail-closed reason: {rejection}"
            );
        }
    }

    /// A target the session controller does not hold must be rejected before
    /// the call is resolved, authorized or executed, even though the workspace
    /// registry advertises the tool.
    #[tokio::test]
    async fn subagent_tools_are_rejected_for_a_target_the_session_does_not_own() {
        let temp = assert_fs::TempDir::new().expect("temporary workspace");
        let registry = diapason_core::tools::registry::ToolRegistry::new(temp.path().to_path_buf()).await;
        let mut vt_cfg = diapason_core::config::DiapasonConfig::default();
        vt_cfg.subagents.enabled = true;
        vt_cfg.subagents.background.auto_restore = false;
        let controller = diapason_core::subagents::SubagentController::new_with_background_owner(
            diapason_core::subagents::SubagentControllerConfig {
                workspace_root: temp.path().to_path_buf(),
                parent_session_id: "session-a".to_string(),
                parent_model: "gpt-5.6-sol".to_string(),
                parent_provider: "openai".to_string(),
                parent_reasoning_effort: diapason_core::config::types::ReasoningEffortLevel::Low,
                api_key: String::new(),
                vt_cfg,
                openai_chatgpt_auth: None,
                depth: 0,
                workspace_gated: false,
                exec_sessions: registry.exec_session_manager(),
                output_reader: None,
                pty_manager: registry.pty_manager().clone(),
                managed_background_runtime: false,
                lock_scope: None,
            },
            Some("session-a"),
        )
        .await
        .expect("session-scoped controller");
        let controller = Arc::new(controller);

        let unowned = r#"{"action":"send_input","id":"not-mine"}"#;
        // The pre-hook check only requires the controller: the raw target may
        // be obsolete, and a permission hook may replace it before dispatch.
        assert!(
            session_subagent_controller_error(tools::AGENT, Some(&controller)).is_none(),
            "the early check must not reject an unowned raw target"
        );
        let rejection = session_subagent_admission_error(tools::AGENT, Some(&controller), unowned)
            .await
            .expect("the post-rewrite check must reject a target the session does not own");
        assert!(rejection.contains("not-mine"), "the rejection must name the target: {rejection}");
    }

    #[tokio::test]
    async fn ordinary_tools_do_not_need_a_session_controller() {
        let rejection = session_subagent_admission_error("read_file", None, "{}").await;
        assert!(rejection.is_none(), "a non-subagent tool must not be tied to a controller");
    }

    /// Target extraction mirrors the core executor: `wait` carries `ids`,
    /// `send_input`/`resume`/`close` carry `id`, and `spawn` carries neither.
    /// A legacy call without `action` is inferred from its argument shape.
    #[test]
    fn unified_agent_targets_follow_the_dispatched_action() {
        let targets = |arguments: Value| subagent_call_targets(tools::AGENT, &arguments.to_string());

        assert_eq!(targets(json!({"action": "send_input", "id": "child-1"})), vec!["child-1"]);
        assert_eq!(targets(json!({"action": "resume", "id": "child-1"})), vec!["child-1"]);
        assert_eq!(targets(json!({"action": "close", "id": "child-1"})), vec!["child-1"]);
        assert_eq!(targets(json!({"action": "wait", "ids": ["a", "b"]})), vec!["a", "b"]);
        assert!(targets(json!({"action": "spawn", "agent": "worker"})).is_empty());
        assert_eq!(targets(json!({"id": "child-1", "message": "go"})), vec!["child-1"]);
        assert_eq!(targets(json!({"id": "child-1"})), vec!["child-1"]);
        assert!(targets(json!({"message": "go"})).is_empty());
    }

    #[test]
    fn malformed_arguments_name_no_target() {
        assert!(subagent_call_targets(tools::AGENT, "not json").is_empty());
    }
}

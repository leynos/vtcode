//! Admission of subagent tool calls against the session's own controller.
//!
//! The advertised tool list can come from the workspace registry, which knows
//! the workspace controller, while a call must execute through the session's
//! controller. These checks reject a call the session cannot own before it is
//! resolved, authorized or executed.

use diapason_core::config::constants::tools;
use serde_json::Value;
use std::sync::Arc;

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
pub(super) async fn session_subagent_admission_error(
    tool_name: &str,
    controller: Option<&Arc<diapason_core::subagents::SubagentController>>,
    arguments: &str,
) -> Option<String> {
    if !diapason_core::subagents::is_subagent_tool(tool_name) {
        return None;
    }
    let Some(controller) = controller else {
        // The session controller's absence is reported with the core
        // registry's established wording, so the client and model see the
        // same fail-closed reason on both enforcement paths.
        return Some(format!(
            "Subagent controller is not active in this session: tool '{tool_name}' has no session-owned controller"
        ));
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
    match tool_name {
        tools::SEND_INPUT | tools::RESUME_AGENT | tools::CLOSE_AGENT => {
            args.get("id").and_then(Value::as_str).map(str::to_owned).into_iter().collect()
        }
        tools::WAIT_AGENT => string_array(&args, "ids"),
        tools::AGENT => {
            let action = args
                .get("action")
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
                .unwrap_or_else(|| {
                    let has_id = args.get("id").is_some();
                    let has_input = args.get("message").is_some() || args.get("items").is_some();
                    match (has_id, has_input) {
                        (true, true) => "send_input".to_string(),
                        (true, false) => "resume".to_string(),
                        _ => "spawn".to_string(),
                    }
                });
            match action.as_str() {
                "send_input" | "resume" | "resume_agent" | "close" => {
                    args.get("id").and_then(Value::as_str).map(str::to_owned).into_iter().collect()
                }
                "wait" => string_array(&args, "ids"),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

fn string_array(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
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
            .background_status_entries()
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

        let rejection = session_subagent_admission_error(
            tools::AGENT,
            Some(&controller),
            r#"{"action":"send_input","id":"not-mine"}"#,
        )
        .await
        .expect("a target the session does not own must be rejected");
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

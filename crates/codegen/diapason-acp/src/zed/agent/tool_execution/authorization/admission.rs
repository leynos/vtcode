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
//! runs after the permission stages — therefore after the initial `ToolCall`
//! notification — and judges per-target ownership on the arguments that will
//! actually execute; without it a rewrite could name a target this session does
//! not own and only core would later refuse the call. It still precedes
//! preapproval, the in-progress notification and dispatch.
//!
//! Admission and execution must answer one question the same way. The command
//! a call dispatches to runs through the session's controller with
//! [`TaskOwner::Session`], which reads only that controller's own child records;
//! a grandchild belongs to the child-scoped controller that spawned it. The
//! ownership check here therefore asks the same controller-local question
//! rather than walking the tree, so a target cannot be admitted and then
//! refused, and one such target cannot fail a whole `wait` list after the call
//! was already admitted.

#[cfg(test)]
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

/// The arguments an admission check judges.
///
/// A call that parsed cleanly is judged on its parsed value: that value is what
/// the executor dispatches, and the ACP model already holds it, so admitting it
/// never costs a second serialization. Only a call whose arguments did not
/// parse is judged on its raw text, where there is no parsed value to judge.
#[derive(Clone, Copy)]
pub(super) enum AdmissionArguments<'a> {
    /// The arguments as parsed, and as a permission rewrite may have replaced
    /// them.
    Parsed(&'a Value),
    /// The model's original argument text, used when parsing failed.
    Raw(&'a str),
}

impl AdmissionArguments<'_> {
    /// The subagent ids these arguments name.
    ///
    /// The tool name decides the target shape, so an ordinary tool yields no
    /// targets at all and its arguments are never read.
    fn targets(self, tool_name: &str) -> Vec<String> {
        match self {
            Self::Parsed(args) => diapason_core::subagents::agent_call_targets(tool_name, args),
            Self::Raw(arguments) => raw_call_targets(tool_name, arguments),
        }
    }
}

/// Rejects a subagent tool call this session cannot own.
///
/// The ACP model advertises tools from the workspace registry, which knows the
/// workspace controller, while a call must execute through the session's own
/// controller. Without this admission step a prompt-driven `agent` call could
/// address a target that another session owns, or run through the workspace's
/// ownerless controller on a session that has no controller of its own. A
/// target-less call (`spawn`) needs only the session controller; a call that
/// names `id`/`ids` additionally needs every named target to be one the command
/// will accept, which is the controller-local set [`session_owns_subagent`]
/// applies.
///
/// This check runs late: the initial `ToolCall` notification has already been
/// published and the permission stages (the `PermissionRequest` hook and, when
/// required, the client prompt) have already resolved, so the refusal is never
/// announced for a call id the client has not seen start. It runs before
/// preapproval, before the in-progress notification and before dispatch, so a
/// refused call is never marked approved or handed to the executor. A `wait`
/// list naming one unaddressable task is therefore rejected as a whole under
/// this reason, after the call was announced, instead of failing later in core
/// after it was admitted.
///
/// It establishes ownership only. A target that passes is one this session may
/// address; whether the command accepts it is a separate question, so a
/// session-owned background record passes admission and may still be rejected
/// by a command that applies only to child tasks.
///
/// This is the post-rewrite check. The caller runs it on the arguments a
/// permission hook installed, so a rewrite cannot smuggle in a foreign target;
/// [`session_subagent_controller_error`] is the early check that runs before a
/// hook can replace the raw arguments.
pub(super) async fn session_subagent_admission_error(
    tool_name: &str,
    controller: Option<&Arc<diapason_core::subagents::SubagentController>>,
    arguments: AdmissionArguments<'_>,
) -> Option<String> {
    if !diapason_core::subagents::is_subagent_tool(tool_name) {
        return None;
    }
    let Some(controller) = controller else {
        return session_subagent_controller_error(tool_name, None);
    };
    for target in arguments.targets(tool_name) {
        if !session_owns_subagent(controller, target.as_str()).await {
            return Some(format!("Tool '{tool_name}' cannot operate on '{target}': it is not owned by this session"));
        }
    }
    None
}

/// Returns the subagent ids a raw argument text names.
///
/// The core executor reports malformed arguments on its own path, so a text
/// that cannot be parsed names no target to admit or reject here. A text that
/// does parse is read through the same parser the executor uses, so an
/// admission decision cannot be made about a different task than the one the
/// executor then addresses.
fn raw_call_targets(tool_name: &str, arguments: &str) -> Vec<String> {
    let Ok(args) = serde_json::from_str::<Value>(arguments) else {
        return Vec::new();
    };
    diapason_core::subagents::agent_call_targets(tool_name, &args)
}

/// Whether `target` is a task this session can address through `controller`.
///
/// The command the call dispatches to executes through this controller with
/// [`TaskOwner::Session`] for the invoking session, and that check reads only
/// this controller's own child records. Admission asks the same controller-local
/// question: a grandchild lives in the child-scoped controller that spawned it,
/// so walking descendants would admit a target the command then refuses, and one
/// refused target inside a `wait` list would fail the whole call after it had
/// been admitted. A direct child is addressable here; a grandchild is not.
///
/// A managed background record this session owns is also admitted. The record is
/// not a child, so the child command reports its own domain error when it
/// executes; admission only has to avoid claiming that a record of this
/// session's is unowned. Reads are pure: `background_status_entries` never
/// refreshes, respawns, persists or publishes.
async fn session_owns_subagent(controller: &Arc<diapason_core::subagents::SubagentController>, target: &str) -> bool {
    let session_id = controller.served_session_id().await;
    if controller.owns_child(&session_id, target).await {
        return true;
    }
    controller
        .background_status_entries(TaskOwner::Session(session_id.as_str()))
        .await
        .iter()
        .any(|entry| entry.id == target)
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
            let rejection = session_subagent_admission_error(tool, None, AdmissionArguments::Raw("{}"))
                .await
                .expect("a subagent tool must not run without a session controller");
            assert!(
                rejection.contains("Subagent controller is not active in this session"),
                "the rejection must carry the established fail-closed reason: {rejection}"
            );
        }
    }

    /// A target the session controller does not hold must be rejected before
    /// the call is preapproved, announced as in progress or executed, even
    /// though the workspace registry advertises the tool.
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
        let rejection =
            session_subagent_admission_error(tools::AGENT, Some(&controller), AdmissionArguments::Raw(unowned))
                .await
                .expect("the post-rewrite check must reject a target the session does not own");
        assert!(rejection.contains("not-mine"), "the rejection must name the target: {rejection}");
    }

    /// Builds a controller config for a fixture tree that never launches a run.
    fn fixture_controller_config(
        workspace: &std::path::Path,
        vt_cfg: &diapason_core::config::DiapasonConfig,
        parent_session_id: &str,
        depth: usize,
        managed_background_runtime: bool,
        registry: &diapason_core::tools::registry::ToolRegistry,
    ) -> diapason_core::subagents::SubagentControllerConfig {
        diapason_core::subagents::SubagentControllerConfig {
            workspace_root: workspace.to_path_buf(),
            parent_session_id: parent_session_id.to_string(),
            parent_model: "gpt-5.6-sol".to_string(),
            parent_provider: "openai".to_string(),
            parent_reasoning_effort: diapason_core::config::types::ReasoningEffortLevel::Low,
            api_key: String::new(),
            vt_cfg: vt_cfg.clone(),
            openai_chatgpt_auth: None,
            depth,
            workspace_gated: false,
            exec_sessions: registry.exec_session_manager(),
            output_reader: None,
            pty_manager: registry.pty_manager().clone(),
            managed_background_runtime,
            lock_scope: None,
        }
    }

    /// Admission must judge a nested-delegation target exactly as the command
    /// it guards does.
    ///
    /// A grandchild lives in the child-scoped controller that spawned it, while
    /// the session-scoped commands read only the session controller's own
    /// records. Admission used to walk `descendant_controllers()`, so it
    /// admitted a grandchild that execution then refused, and one unaddressable
    /// target in a mixed `wait` list failed the whole call. The controller-local
    /// set now gives both layers the same answer.
    ///
    /// The fixture setup is boxed: it holds a whole controller configuration and
    /// two controller constructions, so leaving it inline would make the test's
    /// future large enough to trip `clippy::large_futures`.
    #[tokio::test]
    async fn nested_delegation_admission_matches_the_command_it_guards() {
        Box::pin(nested_delegation_admission_scenario()).await;
    }

    /// The fixture body, kept out of the test's own future.
    async fn nested_delegation_admission_scenario() {
        let temp = assert_fs::TempDir::new().expect("nested delegation workspace");
        let registry = diapason_core::tools::registry::ToolRegistry::new(temp.path().to_path_buf()).await;
        let mut vt_cfg = diapason_core::config::DiapasonConfig::default();
        vt_cfg.subagents.enabled = true;
        // The fixture tree holds no managed task, so the session controller
        // must not attempt a workspace-ledger restore.
        vt_cfg.subagents.background.auto_restore = false;
        let root = Arc::new(
            diapason_core::subagents::SubagentController::new_with_background_owner(
                fixture_controller_config(temp.path(), &vt_cfg, "session-a", 0, false, &registry),
                Some("session-a"),
            )
            .await
            .expect("session controller"),
        );
        let nested = Arc::new(
            diapason_core::subagents::SubagentController::new(fixture_controller_config(
                temp.path(),
                &vt_cfg,
                "child-session",
                1,
                true,
                &registry,
            ))
            .await
            .expect("child-scoped controller"),
        );
        root.insert_test_child_record("child-task", "child-session", "session-a", 1, "default")
            .await
            .expect("fixture child record");
        nested
            .insert_test_child_record("grandchild-task", "grandchild-session", "child-session", 2, "explorer")
            .await
            .expect("fixture grandchild record");
        root.attach_child_controller("child-task", Arc::clone(&nested))
            .await
            .expect("the child-scoped controller joins the session tree");

        // The tree really reaches the grandchild, which is exactly what the
        // removed descendant walk admitted from.
        assert!(
            root.descendant_controllers()
                .await
                .iter()
                .any(|controller| Arc::ptr_eq(controller, &nested)),
            "the fixture must make the grandchild's controller reachable from the session tree"
        );
        assert!(
            nested.status_entries().await.iter().any(|entry| entry.id == "grandchild-task"),
            "the descendant controller must track the grandchild"
        );

        // A direct child is addressable by both layers.
        assert!(
            session_subagent_admission_error(
                tools::AGENT,
                Some(&root),
                AdmissionArguments::Raw(r#"{"action":"send_input","id":"child-task"}"#),
            )
            .await
            .is_none(),
            "a direct child of the session must be admitted"
        );
        assert!(
            root.status_for(TaskOwner::Session("session-a"), "child-task").await.is_ok(),
            "the command the admitted call reaches must accept the direct child"
        );

        // Admission refuses a grandchild exactly as the command does.
        let rejection = session_subagent_admission_error(
            tools::AGENT,
            Some(&root),
            AdmissionArguments::Raw(r#"{"action":"send_input","id":"grandchild-task"}"#),
        )
        .await
        .expect("a grandchild tracked by a descendant controller is not addressable here");
        assert!(rejection.contains("grandchild-task"), "the rejection must name the target: {rejection}");
        assert!(
            root.status_for(TaskOwner::Session("session-a"), "grandchild-task")
                .await
                .is_err(),
            "the command refuses a grandchild through the session controller, so admission must too"
        );

        // One unaddressable target refuses the whole list before execution,
        // rather than after the call was announced and failed whole.
        let mixed = session_subagent_admission_error(
            tools::AGENT,
            Some(&root),
            AdmissionArguments::Raw(r#"{"action":"wait","ids":["child-task","grandchild-task"]}"#),
        )
        .await
        .expect("one unaddressable target must refuse the whole wait list");
        assert!(mixed.contains("grandchild-task"), "the refusal must name the offending target: {mixed}");

        nested.signal_shutdown().await;
        root.signal_shutdown().await;
    }

    #[tokio::test]
    async fn ordinary_tools_do_not_need_a_session_controller() {
        let rejection = session_subagent_admission_error("read_file", None, AdmissionArguments::Raw("{}")).await;
        assert!(rejection.is_none(), "a non-subagent tool must not be tied to a controller");
    }

    /// Target extraction mirrors the core executor: `wait` carries `ids`,
    /// `send_input`/`resume`/`close` carry `id`, and `spawn` carries neither.
    /// A legacy call without `action` is inferred from its argument shape.
    #[test]
    fn unified_agent_targets_follow_the_dispatched_action() {
        let targets = |arguments: Value| AdmissionArguments::Parsed(&arguments).targets(tools::AGENT);

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
        assert!(raw_call_targets(tools::AGENT, "not json").is_empty());
    }

    /// An ordinary tool's arguments are never read, whichever form they take.
    ///
    /// Admission is asked about every dispatched call, so a check that read an
    /// ordinary tool's arguments would pay for target extraction on every file
    /// read. The tool name decides the target shape, and a non-subagent name
    /// yields none, so neither a parsed value nor a raw text can name a target
    /// for one.
    #[test]
    fn ordinary_tools_name_no_targets_from_either_argument_form() {
        let parsed = json!({"action": "send_input", "id": "looks-like-a-target"});
        assert!(
            AdmissionArguments::Parsed(&parsed).targets("read_file").is_empty(),
            "a parsed ordinary-tool argument set must not be mined for targets"
        );
        assert!(
            AdmissionArguments::Raw(r#"{"action":"send_input","id":"looks-like-a-target"}"#)
                .targets("read_file")
                .is_empty(),
            "a raw ordinary-tool argument text must not be mined for targets"
        );
    }

    /// Admission must judge the same arguments the executor dispatches.
    ///
    /// A call that parsed is judged on its parsed value, which is exactly what
    /// the executor receives; rendering it back to text first would re-serialize
    /// every ordinary tool call for a check that reads no target from it. A call
    /// that never parsed has no parsed value, so it is judged on the raw text
    /// and the executor still reports the malformed arguments itself.
    #[test]
    fn admission_judges_the_value_the_executor_dispatches() {
        let token = "target-only-a-rewrite-can-name";
        let parsed = json!({"action": "send_input", "id": token});

        assert_eq!(
            AdmissionArguments::Parsed(&parsed).targets(tools::AGENT),
            vec![token],
            "a parsed call must be judged on its parsed value"
        );
        // The same call as text is judged identically, because the raw form
        // parses and then reads the parsed value.
        assert_eq!(
            AdmissionArguments::Raw(&parsed.to_string()).targets(tools::AGENT),
            vec![token],
            "a raw argument text that parses must reach the same decision"
        );
        assert!(
            AdmissionArguments::Raw("{not json").targets(tools::AGENT).is_empty(),
            "an unparsable text names no target, leaving the diagnostic to the executor"
        );
    }
}

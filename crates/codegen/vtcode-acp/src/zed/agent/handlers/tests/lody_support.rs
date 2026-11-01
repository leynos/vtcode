//! Shared fixtures and request helpers for the Lody management protocol tests.
use super::*;

// ─── Nested delegation over ACP ─────────────────────────────────────────────

/// Provider used by the child and grandchild runners in the nested-management
/// regression.
///
/// Child runners resolve providers through the process-global
/// `create_provider_with_config`, not through the ACP prompt override, so the
/// mock has to be registered in the global factory for the duration of the
/// test.
pub(super) const NESTED_PROVIDER: &str = "nested-delegation-mock";
pub(super) const NESTED_MODEL: &str = vtcode_config::constants::models::anthropic::CLAUDE_SONNET_5;
/// Task handed to the `default` child.
pub(super) const NESTED_CHILD_TASK: &str = "Survey the delegated fixture and report readiness.";
/// Task the child delegates to its read-only grandchild.
pub(super) const NESTED_GRANDCHILD_TASK: &str = "grandchild-marker: list every source file and report each path.";

/// Removes the mock provider from the process-global factory on drop.
pub(super) struct GlobalProviderRegistration(pub(super) &'static str);

impl Drop for GlobalProviderRegistration {
    fn drop(&mut self) {
        if let Ok(mut factory) = vtcode_core::llm::factory::get_factory().lock() {
            factory.remove_provider(self.0);
        }
    }
}

/// Provider that never answers, parking every child run it serves.
///
/// Parking instead of completing keeps the nested tree alive and observable
/// while the test inspects it; the ACP cancel aborts the parked futures.
pub(super) struct ParkedProvider;

#[async_trait]
impl LLMProvider for ParkedProvider {
    fn name(&self) -> &str {
        NESTED_PROVIDER
    }

    fn supported_models(&self) -> Vec<String> {
        vec![NESTED_MODEL.to_string()]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        std::future::pending().await
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        std::future::pending().await
    }
}

pub(super) async fn lody_list(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    active_only: bool,
) -> Result<serde_json::Value, acp::Error> {
    let params = serde_json::value::to_raw_value(&serde_json::json!({
        "sessionId": session_id,
        "activeOnly": active_only,
    }))
    .expect("serialize Lody list request");
    connection
        .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
            crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
            params.into(),
        )))
        .block_task()
        .await
}

pub(super) async fn lody_output(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    task_id: &str,
) -> Result<serde_json::Value, acp::Error> {
    let params = serde_json::value::to_raw_value(&serde_json::json!({
        "sessionId": session_id,
        "taskId": task_id,
        "tail": 20,
    }))
    .expect("serialize Lody output request");
    connection
        .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
            crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
            params.into(),
        )))
        .block_task()
        .await
}

pub(super) async fn lody_cancel(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    task_id: &str,
) -> Result<serde_json::Value, acp::Error> {
    let params = serde_json::value::to_raw_value(&serde_json::json!({
        "sessionId": session_id,
        "taskId": task_id,
        "reason": "nested management regression",
    }))
    .expect("serialize Lody cancel request");
    connection
        .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
            crate::zed::agent::lody::LODY_SUBAGENTS_CANCEL_METHOD,
            params.into(),
        )))
        .block_task()
        .await
}

/// Extracts the Lody task metadata from a task-lifecycle session notification.
pub(super) fn lifecycle_task_meta(notification: &acp::AgentNotification) -> Option<serde_json::Value> {
    let acp::AgentNotification::SessionNotification(notification) = notification else {
        return None;
    };
    let meta = match &notification.update {
        acp::SessionUpdate::ToolCall(call) => call.meta.as_ref(),
        acp::SessionUpdate::ToolCallUpdate(update) => update.meta.as_ref(),
        _ => None,
    }?;
    let task = &meta["lody"]["task"];
    task.get("taskId")?.as_str().map(|_| task.clone())
}

/// Extracts the Lody task ID from a task-lifecycle session notification.
pub(super) fn lifecycle_task_id(notification: &acp::AgentNotification) -> Option<String> {
    lifecycle_task_meta(notification)?.get("taskId")?.as_str().map(str::to_owned)
}

pub(super) async fn lody_output_error(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    task_id: &str,
) -> acp::Error {
    let params = serde_json::value::to_raw_value(&serde_json::json!({
        "sessionId": session_id,
        "taskId": task_id,
    }))
    .expect("serialize Lody output request");
    connection
        .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
            crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
            params.into(),
        )))
        .block_task()
        .await
        .expect_err("Lody output request should fail")
}

pub(super) fn write_lody_background_fixture(workspace: &std::path::Path) {
    let agent_dir = workspace.join(".vtcode/agents");
    std::fs::create_dir_all(&agent_dir).expect("create Lody test agent directory");
    std::fs::write(
        agent_dir.join("background-demo.md"),
        r#"---
name: background-demo
description: Lody management protocol fixture.
tools:
  - command_session
background: true
maxTurns: 2
initialPrompt: Report readiness once.
---

Run the managed background fixture.
"#,
    )
    .expect("write Lody test agent");
    let scripts_dir = workspace.join("scripts");
    std::fs::create_dir_all(&scripts_dir).expect("create Lody test scripts directory");
    let script = scripts_dir.join("demo-background-subagent.sh");
    std::fs::write(&script, "#!/bin/sh\nprintf 'lody-output-one\\nlody-output-two\\n'\nsleep 30\n")
        .expect("write Lody background script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script)
            .expect("Lody background script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script, permissions).expect("make Lody background script executable");
    }
}

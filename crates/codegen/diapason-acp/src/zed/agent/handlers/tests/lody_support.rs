//! Shared fixtures and request helpers for the Lody management protocol tests.
use super::*;
use agent_client_protocol::schema::v1::{
    CloseSessionRequest, InitializeRequest, NewSessionRequest, NewSessionResponse, SetSessionConfigOptionRequest,
};

// ─── Duplex connection plumbing ─────────────────────────────────────────────

/// Serves the agent half of a duplex connection over the supplied agent.
///
/// The returned handle owns the client half, and the join handle stops the
/// server task when a test finishes.
pub(super) fn spawn_agent_connection(
    agent: &Arc<ZedAgent>,
    name: &'static str,
) -> (Channel, tokio::task::JoinHandle<agent_client_protocol::Result<()>>) {
    let (agent_channel, client_channel) = Channel::duplex();
    let connection = install_handlers(Agent.builder().name(name), Arc::clone(agent)).connect_with(agent_channel, {
        let agent = Arc::clone(agent);
        async move |cx: ConnectionTo<Client>| {
            agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
            std::future::pending::<agent_client_protocol::Result<()>>().await
        }
    });
    (client_channel, tokio::spawn(connection))
}

/// Initializes the connection and opens a session on the `build` primary agent.
pub(super) async fn open_build_session(
    cx: &ConnectionTo<Agent>,
    workspace: std::path::PathBuf,
) -> Result<NewSessionResponse, acp::Error> {
    drop(
        cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
            .block_task()
            .await?,
    );
    let session = cx.send_request(NewSessionRequest::new(workspace)).block_task().await?;
    drop(
        cx.send_request(SetSessionConfigOptionRequest::new(session.session_id.clone(), "primary_agent", "build"))
            .block_task()
            .await?,
    );
    Ok(session)
}

/// Closes an ACP session, waiting for the acknowledgement.
pub(super) async fn close_session(cx: &ConnectionTo<Agent>, session_id: acp::SessionId) -> Result<(), acp::Error> {
    drop(cx.send_request(CloseSessionRequest::new(session_id)).block_task().await?);
    Ok(())
}

// ─── Nested delegation over ACP ─────────────────────────────────────────────

/// Provider used by the child and grandchild runners in the nested-management
/// regression.
///
/// Child runners resolve providers through the process-global
/// `create_provider_with_config`, not through the ACP prompt override, so the
/// mock has to be registered in the global factory for the duration of the
/// test.
pub(super) const NESTED_PROVIDER: &str = "nested-delegation-mock";
pub(super) const NESTED_MODEL: &str = diapason_config::constants::models::anthropic::CLAUDE_SONNET_5;
/// Task handed to the `default` child.
pub(super) const NESTED_CHILD_TASK: &str = "Survey the delegated fixture and report readiness.";
/// Task the child delegates to its read-only grandchild.
pub(super) const NESTED_GRANDCHILD_TASK: &str = "grandchild-marker: list every source file and report each path.";

/// Removes the mock provider from the process-global factory on drop.
pub(super) struct GlobalProviderRegistration(pub(super) &'static str);

impl Drop for GlobalProviderRegistration {
    fn drop(&mut self) {
        if let Ok(mut factory) = diapason_core::llm::factory::get_factory().lock() {
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

/// Sends one Lody management extension request with caller-supplied params.
pub(super) async fn lody_ext(
    connection: &ConnectionTo<Agent>,
    method: &'static str,
    params: serde_json::Value,
) -> Result<serde_json::Value, acp::Error> {
    let params = serde_json::value::to_raw_value(&params).expect("serialize Lody management request");
    connection
        .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(method, params.into())))
        .block_task()
        .await
}

pub(super) async fn lody_list(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    active_only: bool,
) -> Result<serde_json::Value, acp::Error> {
    lody_ext(
        connection,
        crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
        serde_json::json!({ "sessionId": session_id, "activeOnly": active_only }),
    )
    .await
}

pub(super) async fn lody_output(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    task_id: &str,
) -> Result<serde_json::Value, acp::Error> {
    lody_output_tail(connection, session_id, task_id, 20).await
}

/// Requests an output tail of exactly `tail` lines.
pub(super) async fn lody_output_tail(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    task_id: &str,
    tail: usize,
) -> Result<serde_json::Value, acp::Error> {
    lody_ext(
        connection,
        crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
        serde_json::json!({ "sessionId": session_id, "taskId": task_id, "tail": tail }),
    )
    .await
}

pub(super) async fn lody_cancel(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    task_id: &str,
) -> Result<serde_json::Value, acp::Error> {
    lody_cancel_with_reason(connection, session_id, task_id, "nested management regression").await
}

/// Requests cancellation with an explicit reason.
pub(super) async fn lody_cancel_with_reason(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    task_id: &str,
    reason: &str,
) -> Result<serde_json::Value, acp::Error> {
    lody_ext(
        connection,
        crate::zed::agent::lody::LODY_SUBAGENTS_CANCEL_METHOD,
        serde_json::json!({ "sessionId": session_id, "taskId": task_id, "reason": reason }),
    )
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
    lody_ext(
        connection,
        crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
        serde_json::json!({ "sessionId": session_id, "taskId": task_id }),
    )
    .await
    .expect_err("Lody output request should fail")
}

/// Reads a workspace runtime's live session count under its lifecycle lock.
pub(super) fn runtime_active_sessions(runtime: &crate::zed::agent::SessionWorkspaceRuntime) -> usize {
    runtime
        .lifecycle
        .lock()
        .expect("workspace runtime lifecycle lock")
        .active_sessions
}

/// Projects a controller timestamp exactly as the ACP management transport does.
pub(super) fn expected_epoch_seconds(timestamp: chrono::DateTime<chrono::Utc>) -> f64 {
    timestamp.timestamp_millis().max(0) as f64 / 1_000.0
}

/// Observable state of one background record, read without refreshing it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ObservedBackgroundState {
    pub(super) active: bool,
    pub(super) updated_at_millis: i64,
}

/// Reads a record's observable state without refreshing the controller.
///
/// The management tests probe this on both sides of a read request. Because
/// the probe itself never refreshes or mutates, a difference between the two
/// samples can only come from the request under test.
pub(super) async fn background_record_state(
    controller: &diapason_core::subagents::SubagentController,
    task_id: &str,
) -> Option<ObservedBackgroundState> {
    controller
        .background_status_entries()
        .await
        .into_iter()
        .find(|entry| entry.id == task_id)
        .map(|entry| ObservedBackgroundState {
            active: entry.status.is_active(),
            updated_at_millis: entry.updated_at.timestamp_millis(),
        })
}

pub(super) fn write_lody_background_fixture(workspace: &std::path::Path) {
    let agent_dir = workspace.join(".diapason/agents");
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

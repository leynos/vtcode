//! Reporting of the optional components a session sets up for itself.
use super::super::super::optional_setup::{OptionalSetupOutcome, OptionalWorkspaceSetupOutcomes};
use super::super::super::session_workspace::{WorkspaceRuntimeLifecycle, WorkspaceRuntimeLifecycleState};
use super::*;
use crate::zed::agent::test_log_capture::capturing_async;
use assert_fs::TempDir;
use std::sync::Arc;
use tracing_subscriber::filter::LevelFilter;

/// Building a session controller must report only that component.
///
/// A session controller is the only optional component this path sets up, but
/// reporting through the two-component helper invented an MCP outcome. Every
/// session then logged "MCP client disabled by configuration", whatever the
/// real MCP client state was, which hides a genuine MCP failure behind a
/// routine-looking message.
#[tokio::test]
async fn session_controller_setup_reports_only_the_controller_outcome() {
    let temp = TempDir::new().unwrap();
    let mut vt_config = diapason_core::config::DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.auto_restore = false;
    let agent = build_agent_with_vt_config(temp.path(), "duck", Some(&vt_config)).await;

    let session_workspace = TempDir::new().unwrap();
    let registry = diapason_core::tools::registry::ToolRegistry::new(session_workspace.path().to_path_buf()).await;
    let workspace_runtime = SessionWorkspaceRuntime {
        workspace_root: session_workspace.path().to_path_buf(),
        workspace_hooks_gated: false,
        system_prompt: String::new(),
        primary_agents: agent.primary_agents.clone(),
        acp_tool_registry: Arc::clone(&agent.acp_tool_registry),
        permission_prompter: Arc::clone(&agent.permission_prompter),
        local_tool_registry: registry,
        file_ops_tool: None,
        optional_setup_outcomes: OptionalWorkspaceSetupOutcomes {
            subagent_controller: OptionalSetupOutcome::DisabledByConfiguration,
            mcp_client: OptionalSetupOutcome::DisabledByConfiguration,
        },
        shutdown_managed: false,
        lifecycle: Mutex::new(WorkspaceRuntimeLifecycle {
            state: WorkspaceRuntimeLifecycleState::Open,
            active_sessions: 0,
        }),
        lifecycle_changed: tokio::sync::Notify::new(),
    };
    let session_id = acp::SessionId::new("session-controller-reporting");

    let (_, events) = capturing_async(
        LevelFilter::DEBUG,
        agent.build_session_subagent_controller(&workspace_runtime, &session_id, true),
    )
    .await;

    let reported = events.join("\n");
    assert!(
        reported.contains("component=\"subagent controller\""),
        "the controller outcome must be reported: {reported}"
    );
    assert!(
        !reported.contains("MCP client"),
        "a controller-only setup must not report an MCP outcome: {reported}"
    );
}

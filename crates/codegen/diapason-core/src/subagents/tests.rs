//! Tests subagent configuration, execution, lifecycle, and background task behavior.
use super::*;
use crate::config::constants::models;
use crate::config::constants::tools;
use crate::config::models::{ModelId, Provider};
use crate::llm::provider::ToolDefinition;
use crate::llm::provider::{Message, MessageContent, MessageRole};
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::registry::PtySessionManager;
use anyhow::{Result, anyhow};
use diapason_config::core::permissions::{AgentPermissionsConfig, PermissionDefault};
use diapason_config::{
    HookCommandConfig, HookGroupConfig, HooksConfig, IsolationMode, SubagentMcpServer, SubagentMemoryScope,
    SubagentSource, SubagentSpec,
};
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::Notify;

fn readonly_agent_permissions() -> AgentPermissionsConfig {
    let mut permissions = AgentPermissionsConfig::new(PermissionDefault::Deny);
    permissions.allow = vec![tools::READ_FILE.to_string()];
    permissions
}

fn test_controller_config(workspace_root: PathBuf, vt_cfg: DiapasonConfig) -> SubagentControllerConfig {
    let pty_sessions = PtySessionManager::new(workspace_root.clone(), vt_cfg.pty.clone());
    let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
    SubagentControllerConfig {
        workspace_root,
        parent_session_id: "parent-session".to_string(),
        parent_model: models::openai::GPT_5_6_SOL.to_string(),
        parent_provider: "openai".to_string(),
        parent_reasoning_effort: ReasoningEffortLevel::Medium,
        api_key: "test-key".to_string(),
        vt_cfg,
        openai_chatgpt_auth: None,
        depth: 0,
        workspace_gated: false,
        exec_sessions,
        output_reader: None,
        pty_manager: pty_sessions.manager().clone(),
        managed_background_runtime: false,
        lock_scope: None,
    }
}

pub(super) fn persisted_background_record_for_owner(
    id: &str,
    owner_session_id: Option<&str>,
) -> PersistedBackgroundRecord {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "agent_name": "worker",
        "display_label": "worker",
        "description": "worker",
        "source": "builtin",
        "color": null,
        "owner_session_id": owner_session_id,
        "session_id": format!("session-{id}"),
        "exec_session_id": format!("exec-{id}"),
        "desired_enabled": false,
        "status": "stopped",
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": "2026-08-28T00:00:00Z",
        "started_at": null,
        "ended_at": "2026-08-28T00:00:00Z",
        "pid": null,
        "prompt": "work",
        "summary": null,
        "error": null,
        "archive_path": null,
        "transcript_path": null,
        "max_turns": null,
        "model_override": null,
        "reasoning_override": null,
        "restart_attempts": 0
    }))
    .expect("persisted background record")
}

fn test_child_record(
    id: &str,
    session_id: &str,
    parent_thread_id: &str,
    spec: &SubagentSpec,
    status: SubagentStatus,
    depth: usize,
    child_controller: Option<Arc<SubagentController>>,
) -> ChildRecord {
    ChildRecord {
        id: id.to_string(),
        session_id: session_id.to_string(),
        parent_thread_id: parent_thread_id.to_string(),
        spec: spec.clone(),
        display_label: subagent_display_label(spec),
        status,
        background: false,
        depth,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        completed_at: status.is_terminal().then_some(Utc::now()),
        summary: None,
        error: None,
        archive_metadata: None,
        archive_path: None,
        transcript_path: None,
        effective_config: Some(DiapasonConfig::default()),
        stored_messages: Vec::new(),
        last_prompt: Some(format!("prompt-{id}")),
        queued_prompts: VecDeque::new(),
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        thread_handle: None,
        handle: None,
        notify: Arc::new(Notify::new()),
        worktree_path: None,
        child_controller,
        closed: false,
    }
}

mod background_command_tests;
mod background_ledger_lock_tests;
mod background_liveness_fault_tests;
mod background_liveness_launch_tests;
mod background_liveness_projection_tests;
mod background_liveness_support;
mod background_output_query_tests;
mod child_config_tests;
mod child_lifecycle_tests;
mod child_tool_filter_tests;
mod close_semantics_tests;
mod delegation_selection_tests;
mod grandchild_close_tests;
mod grandchild_resume_tests;
mod invocation_context_tests;
mod lifecycle_gate_tests;
mod lock_scope_tests;
mod memory_appendix_tests;
mod model_resolution_tests;
mod owner_scope_tests;
mod ownership_tests;
mod progress_observer_tests;
mod runtime_override_tests;
mod spawn_background_process_tests;
mod spawn_custom_policy_tests;
mod spawn_custom_tests;
mod spawn_limits_tests;
mod spawn_selection_tests;
mod tree_routing_tests;
mod wait_tests;

fn write_test_background_subagent(workspace_root: &std::path::Path) {
    let agent_dir = workspace_root.join(diapason_commons::workspace_dir_name()).join("agents");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    std::fs::write(
        agent_dir.join("background-demo.md"),
        r#"---
name: background-demo
description: Minimal demo agent for the managed background subprocess flow.
tools:
  - command_session
background: true
maxTurns: 2
initialPrompt: Report readiness once.
---

Run the managed background demo.
"#,
    )
    .expect("write background agent");

    let scripts_dir = workspace_root.join("scripts");
    std::fs::create_dir_all(&scripts_dir).expect("scripts dir");
    let script_path = scripts_dir.join("demo-background-subagent.sh");
    std::fs::write(&script_path, "#!/bin/sh\nexit 0\n").expect("write background script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script_path)
            .expect("background script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("background script permissions");
    }
}

fn write_test_primary_agent(workspace_root: &std::path::Path) {
    let agent_dir = workspace_root.join(diapason_commons::workspace_dir_name()).join("agents");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    std::fs::write(
        agent_dir.join("duck.md"),
        r#"---
name: duck
description: Discussion controller.
mode: primary
permissions:
  default: ask
---

Discuss before implementation.
"#,
    )
    .expect("write primary agent");
}

fn write_test_read_only_subagent(workspace_root: &std::path::Path) {
    let agent_dir = workspace_root.join(diapason_commons::workspace_dir_name()).join("agents");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    std::fs::write(
        agent_dir.join("readonly-demo.md"),
        r#"---
name: readonly-demo
description: Read-only test child agent.
tools:
  - code_search
permissions:
  default: ask
---

Inspect the repository.
"#,
    )
    .expect("write read-only agent");
}

async fn wait_for_effective_model(controller: &SubagentController, target: &str) -> Result<String> {
    for _ in 0..50 {
        if let Ok(snapshot) = controller.snapshot_for_thread(TaskOwner::Workspace, target).await {
            return Ok(snapshot.effective_config.agent.default_model);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    Err(anyhow!("Subagent {target} did not capture an effective runtime configuration in time"))
}

fn read_only_test_spec(name: &str) -> SubagentSpec {
    SubagentSpec {
        name: name.to_string(),
        description: "test".to_string(),
        prompt: String::new(),
        tools: Some(vec![tools::READ_FILE.to_string()]),
        disallowed_tools: Vec::new(),
        model: None,
        colour: None,
        reasoning_effort: None,
        permissions: readonly_agent_permissions(),
        skills: Vec::new(),
        mcp_servers: Vec::new(),
        hooks: None,
        background: false,
        mode: diapason_config::AgentMode::Subagent,
        max_turns: None,
        nickname_candidates: Vec::new(),
        initial_prompt: None,
        memory: None,
        isolation: None,
        aliases: Vec::new(),
        source: SubagentSource::Builtin,
        file_path: None,
        warnings: Vec::new(),
        tool_policy_overrides: BTreeMap::new(),
    }
}

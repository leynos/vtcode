//! Fixtures shared by the background-operations tests.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::config::DiapasonConfig;
use crate::config::constants::models;
use crate::config::types::ReasoningEffortLevel;
use crate::subagents::background::background_state_path_for_tests;
#[cfg(unix)]
use crate::subagents::controller_background_ops::controller_background_liveness::exec_session_is_missing;
use crate::subagents::{
    BackgroundRecord, BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentController,
    SubagentControllerConfig, SubagentProgressObserver, SubagentStatusEntry,
};
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::registry::PtySessionManager;

/// Builds the controller configuration the background tests run against.
pub(super) fn test_controller_config(workspace_root: PathBuf, vt_cfg: DiapasonConfig) -> SubagentControllerConfig {
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

/// Builds a background record for one owner and exec session.
pub(super) fn background_record(
    id: &str,
    owner_session_id: Option<&str>,
    agent_name: &str,
    exec_session_id: &str,
    status: BackgroundSubprocessStatus,
    desired_enabled: bool,
) -> BackgroundRecord {
    BackgroundRecord {
        id: id.to_string(),
        owner_session_id: owner_session_id.map(str::to_owned),
        agent_name: agent_name.to_string(),
        display_label: agent_name.to_string(),
        description: agent_name.to_string(),
        source: "builtin".to_string(),
        colour: None,
        session_id: format!("session-{id}"),
        parent_session_id: "parent-session".to_string(),
        exec_session_id: exec_session_id.to_string(),
        desired_enabled,
        status,
        created_at: "2026-08-28T00:00:00Z".parse().expect("created_at"),
        updated_at: "2026-08-28T00:00:00Z".parse().expect("updated_at"),
        started_at: None,
        ended_at: None,
        pid: None,
        prompt: "work".to_string(),
        summary: None,
        error: None,
        archive_path: None,
        transcript_path: None,
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        restart_attempts: 0,
    }
}

/// Writes a one-record background ledger and returns its path.
pub(super) fn write_ledger(
    workspace_root: &Path,
    id: &str,
    owner_session_id: Option<&str>,
    agent_name: &str,
    exec_session_id: &str,
    desired_enabled: bool,
    status: &str,
) -> PathBuf {
    let ledger = background_state_path_for_tests(workspace_root);
    if let Some(parent) = ledger.parent() {
        std::fs::create_dir_all(parent).expect("state dir");
    }
    let record = serde_json::json!({
        "id": id,
        "agent_name": agent_name,
        "display_label": agent_name,
        "description": agent_name,
        "source": "builtin",
        "color": null,
        "owner_session_id": owner_session_id,
        "session_id": format!("session-{id}"),
        "exec_session_id": exec_session_id,
        "desired_enabled": desired_enabled,
        "status": status,
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": "2026-08-28T00:00:00Z",
        "started_at": null,
        "ended_at": null,
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
    });
    std::fs::write(
        &ledger,
        serde_json::to_string(&serde_json::json!({ "records": [record] })).expect("serialize ledger"),
    )
    .expect("write ledger");
    ledger
}

/// Writes the demo agent and its launch script into a workspace.
///
/// The only fixtures that launch the demo agent drive a real child process,
/// which the background launch path supports on Unix only, so the helper is
/// gated with its callers. Without the gate a Windows test build compiles it
/// unused and fails the workspace's `-D warnings` dead-code check.
#[cfg(unix)]
pub(crate) fn write_background_demo_agent(workspace_root: &Path) {
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

/// Rewrites the demo launch script so the subprocess stays alive while a test
/// examines the record it produced.
#[cfg(unix)]
pub(crate) fn write_sleeping_demo_script(workspace_root: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let script_path = workspace_root.join("scripts").join("demo-background-subagent.sh");
    std::fs::write(&script_path, "#!/bin/sh\nsleep 30\n").expect("write the sleeping background script");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("background script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script_path, permissions).expect("background script permissions");
}

/// Builds a controller over a fresh workspace with the supplied configuration.
pub(super) async fn background_test_controller(workspace_root: &Path, vt_cfg: DiapasonConfig) -> SubagentController {
    SubagentController::new(test_controller_config(workspace_root.to_path_buf(), vt_cfg))
        .await
        .expect("background controller")
}

/// Starts a real pipe session that stays alive until the test closes it.
#[cfg(unix)]
pub(super) async fn start_sleep_session(controller: &SubagentController, exec_session_id: &str) {
    controller
        .config
        .exec_sessions
        .create_pipe_session(
            exec_session_id.to_string().into(),
            vec!["sh".to_string(), "-c".to_string(), "sleep 30".to_string()],
            controller.config.workspace_root.clone(),
            hashbrown::HashMap::new(),
        )
        .await
        .expect("the sleep session starts");
}

/// Reports whether the exec session is gone rather than unreadable.
///
/// Only the typed "session missing" cause is an exit. Any other read fault
/// leaves the process's fate unknown, so reporting it as gone would let a test
/// pass on evidence the production path deliberately refuses to act on. Such a
/// fault therefore fails the call with its own details rather than returning a
/// misleading boolean.
#[cfg(unix)]
pub(super) async fn session_is_gone(controller: &SubagentController, exec_session_id: &str) -> bool {
    match controller.config.exec_sessions.snapshot_session(exec_session_id).await {
        Ok(_) => false,
        Err(error) if exec_session_is_missing(&error) => true,
        Err(error) => panic!("exec session '{exec_session_id}' could not be read, so its state is unknown: {error:#}"),
    }
}

/// Records the background snapshots one controller publishes.
#[derive(Default)]
pub(super) struct RecordingObserver {
    background: Mutex<Vec<BackgroundSubprocessEntry>>,
}

impl RecordingObserver {
    /// Returns the background snapshots published so far.
    pub(super) fn background_entries(&self) -> Vec<BackgroundSubprocessEntry> {
        self.background
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl SubagentProgressObserver for RecordingObserver {
    fn subagent_status_changed(&self, _entry: SubagentStatusEntry) {}

    fn background_status_changed(&self, entry: BackgroundSubprocessEntry) {
        self.background
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry);
    }
}

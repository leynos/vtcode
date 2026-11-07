//! Workspace lifecycle-hook approval and provenance tests.
use super::*;
use assert_fs::TempDir;
use diapason_config::{HookCommandConfig, HookGroupConfig, WorkspaceHookCommand, WorkspaceLifecycleHooks};
use diapason_core::config::DiapasonConfig;
use std::ffi::OsString;

/// Restores `DIAPASON_CONFIG` when the test leaves scope, including on panic.
struct DiapasonConfigGuard {
    previous: Option<OsString>,
    environment_lock: diapason_commons::env_lock::EnvGuard,
}

impl DiapasonConfigGuard {
    /// Point `DIAPASON_CONFIG` at `config_dir` while holding the process-wide
    /// environment lock, remembering the value to restore.
    fn set(config_dir: &Path) -> Self {
        let environment_lock = diapason_commons::env_lock::lock();
        let previous = std::env::var_os("DIAPASON_CONFIG");
        environment_lock.set_var("DIAPASON_CONFIG", config_dir);
        Self { previous, environment_lock }
    }
}

impl Drop for DiapasonConfigGuard {
    fn drop(&mut self) {
        self.environment_lock.restore_var("DIAPASON_CONFIG", self.previous.as_ref());
    }
}

#[tokio::test]
async fn new_session_restores_matching_workspace_hook_approval_before_session_start() {
    let workspace = TempDir::new().unwrap();
    let dot_config = TempDir::new().unwrap();
    let marker = workspace.path().join("approved-session-start");
    let command = format!("touch {}", marker.display());
    let _config_guard = DiapasonConfigGuard::set(&dot_config.path().join("config"));
    diapason_core::utils::dot_config::initialize_dot_folder().await.unwrap();

    let mut vt_config = DiapasonConfig::default();
    vt_config.hooks.lifecycle.session_start.push(HookGroupConfig {
        matcher: None,
        hooks: vec![HookCommandConfig {
            kind: Default::default(),
            command: command.clone(),
            timeout_seconds: None,
        }],
    });
    vt_config.workspace_lifecycle_hooks = Some(WorkspaceLifecycleHooks {
        commands: vec![WorkspaceHookCommand {
            event: "session_start".to_string(),
            matcher: None,
            command: command.clone(),
            timeout_seconds: None,
        }],
    });
    let preview = LifecycleHookEngine::new_with_session_gated(
        workspace.path().to_path_buf(),
        &vt_config.hooks,
        SessionStartTrigger::NewSession,
        "approval-preview",
        true,
    )
    .unwrap()
    .unwrap();
    diapason_core::utils::dot_config::update_lifecycle_hook_approval(
        workspace.path(),
        preview.command_digest().to_string(),
    )
    .await
    .unwrap();

    let agent = build_agent_with_vt_config(workspace.path(), "duck", Some(&vt_config)).await;
    let result = agent.new_session(acp::NewSessionRequest::new(workspace.path())).await;
    let marker_was_created = marker.exists();

    assert!(result.is_ok(), "session should start after restoring its matching hook approval");
    assert!(marker_was_created, "matching persisted approval must be restored before SessionStart runs");
}

#[tokio::test]
async fn new_session_preserves_workspace_hook_provenance_gate() {
    for (workspace_controlled, expected_gated) in [(true, true), (false, false)] {
        let workspace = TempDir::new().expect("temporary ACP hook workspace");
        let command = ":".to_string();
        let mut config = DiapasonConfig::default();
        config.hooks.lifecycle.session_start = vec![HookGroupConfig {
            matcher: None,
            hooks: vec![HookCommandConfig {
                command: command.clone(),
                ..HookCommandConfig::default()
            }],
        }];
        config.workspace_lifecycle_hooks = Some(if workspace_controlled {
            WorkspaceLifecycleHooks {
                commands: vec![WorkspaceHookCommand {
                    event: "session_start".to_string(),
                    matcher: None,
                    command,
                    timeout_seconds: None,
                }],
            }
        } else {
            WorkspaceLifecycleHooks::default()
        });

        let agent = build_agent_with_vt_config(workspace.path(), "duck", Some(&config)).await;
        let session_id = agent
            .new_session(acp::NewSessionRequest::new(workspace.path()))
            .await
            .expect("construct ACP session with lifecycle hooks")
            .session_id;
        let hooks = agent
            .session_handle(&session_id)
            .expect("ACP session lookup")
            .expect("registered ACP session")
            .lifecycle_hooks()
            .expect("lifecycle hook lookup")
            .expect("configured lifecycle hook engine");

        assert_eq!(
            hooks.workspace_gated(),
            expected_gated,
            "ACP session must preserve whether the loader attributed hooks to the workspace"
        );
    }
}

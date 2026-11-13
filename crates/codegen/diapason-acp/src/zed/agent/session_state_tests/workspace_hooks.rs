//! Workspace lifecycle-hook approval and provenance tests.
use super::*;
use assert_fs::TempDir;
use diapason_config::{HookCommandConfig, HookGroupConfig, WorkspaceHookCommand, WorkspaceLifecycleHooks};
use diapason_core::config::DiapasonConfig;
use std::ffi::OsString;

/// Points the user data roots at `root` for the life of the guard.
///
/// The dot-config manager resolves its roots once per process and caches the
/// manager, so the environment has to be redirected *before* the first lookup.
/// `DIAPASON_CONFIG` is not one of those roots: the path resolver ignores the
/// former application-specific overrides, so setting it would leave both the
/// approval write and the session lookup reading the developer's real
/// `config.toml`.
struct IsolatedUserDataDirs {
    previous: Vec<(&'static str, Option<OsString>)>,
    environment_lock: diapason_commons::env_lock::EnvGuard,
}

impl IsolatedUserDataDirs {
    /// Redirect every canonical XDG root under `root` while holding the
    /// process-wide environment lock, remembering the values to restore.
    fn set(root: &Path) -> Self {
        let environment_lock = diapason_commons::env_lock::lock();
        let mut previous = Vec::with_capacity(4);
        for (variable, directory) in [
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
        ] {
            previous.push((variable, std::env::var_os(variable)));
            environment_lock.set_var(variable, root.join(directory));
        }
        Self { previous, environment_lock }
    }
}

impl Drop for IsolatedUserDataDirs {
    fn drop(&mut self) {
        for (variable, previous) in &self.previous {
            self.environment_lock.restore_var(variable, previous.as_ref());
        }
    }
}

#[tokio::test]
async fn new_session_restores_matching_workspace_hook_approval_before_session_start() {
    let workspace = TempDir::new().expect("temporary ACP hook workspace");
    let user_data = TempDir::new().expect("temporary user data directory");
    let marker = workspace.path().join("approved-session-start");
    let command = format!("touch {}", marker.display());
    // Redirect the user data roots before the first manager lookup: the
    // approval below is written through the process-wide dot-config manager,
    // so without this the test would persist it into the developer's own
    // `config.toml` and the session would read it back from there.
    let _user_data_guard = IsolatedUserDataDirs::set(user_data.path());
    diapason_core::utils::dot_config::initialize_dot_folder()
        .await
        .expect("initialize the dot-config folder for the hook approval");

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
    .expect("build the session-gated lifecycle hook engine")
    .expect("the workspace-controlled hook must produce a preview");
    diapason_core::utils::dot_config::update_lifecycle_hook_approval(
        workspace.path(),
        preview.command_digest().to_string(),
    )
    .await
    .expect("persist the hook approval for the workspace");

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

#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use super::*;
use std::path::Path;
use tempfile::TempDir;

use crate::config::{HookCommandConfig, HookGroupConfig, HooksConfig, LifecycleHooksConfig};
use crate::primary_agent::ActivePrimaryAgent;
use vtcode_config::{SubagentSource, SubagentSpec};

fn session_start_config(commands: &[&str]) -> HooksConfig {
    HooksConfig {
        lifecycle: LifecycleHooksConfig {
            session_start: commands
                .iter()
                .map(|command| HookGroupConfig {
                    matcher: None,
                    hooks: vec![HookCommandConfig {
                        kind: Default::default(),
                        command: (*command).to_string(),
                        timeout_seconds: None,
                    }],
                })
                .collect(),
            ..Default::default()
        },
    }
}

fn build_engine(workspace: &Path, config: &HooksConfig, workspace_gated: bool) -> LifecycleHookEngine {
    LifecycleHookEngine::new_with_session_gated(
        workspace.to_path_buf(),
        config,
        SessionStartTrigger::Startup,
        "test-session",
        workspace_gated,
    )
    .expect("engine constructs")
    .expect("non-empty hooks config produces an engine")
}

#[tokio::test]
async fn ungated_engine_runs_hooks_without_approval() {
    let temp_dir = TempDir::new().expect("tempdir");
    let workspace = temp_dir.path();
    let marker = workspace.join("vt-ungated-marker");
    let command = format!("cat >/dev/null; touch {}", marker.display());

    // User-level-only hook config: no workspace-controlled content, no gate.
    let engine = build_engine(workspace, &session_start_config(&[&command]), false);

    assert!(!engine.workspace_gated());
    assert!(!engine.workspace_hooks_need_approval().await);

    engine.run_session_start().await.expect("run session start");
    assert!(marker.exists(), "ungated hooks must run as before");
}

#[tokio::test]
async fn gated_engine_skips_all_hooks_until_approved() {
    let temp_dir = TempDir::new().expect("tempdir");
    let workspace = temp_dir.path();
    let user_marker = workspace.join("vt-user-marker");
    let workspace_marker = workspace.join("vt-workspace-marker");
    let user_command = format!("cat >/dev/null; touch {}", user_marker.display());
    let workspace_command = format!("cat >/dev/null; touch {}", workspace_marker.display());

    // Both user- and workspace-sourced commands live in the same engine; when
    // workspace-controlled hook content is present, the whole engine is gated
    // (GHSA-wqgw-crr5-cr2p): nothing runs until the exact command set is
    // approved, because the user cannot distinguish the two at a glance.
    let engine = build_engine(workspace, &session_start_config(&[&user_command, &workspace_command]), true);

    assert!(engine.workspace_gated());
    assert!(engine.workspace_hooks_need_approval().await);

    let outcome = engine.run_session_start().await.expect("run session start");
    assert!(
        !user_marker.exists() && !workspace_marker.exists(),
        "no lifecycle hook may run while the workspace gate is unapproved"
    );
    assert!(
        outcome.messages.iter().any(|message| message.text.contains("not approved")),
        "skip reason should be surfaced, got: {:?}",
        outcome.messages
    );

    engine.approve_workspace_hooks().await;
    assert!(!engine.workspace_hooks_need_approval().await);
    engine.run_session_start().await.expect("run session start");
    assert!(user_marker.exists() && workspace_marker.exists(), "approved engines run their exact command set");
}

#[tokio::test]
async fn changed_commands_invalidate_prior_approval() {
    let temp_dir = TempDir::new().expect("tempdir");
    let workspace = temp_dir.path();
    let first_marker = workspace.join("vt-first-marker");
    let second_marker = workspace.join("vt-second-marker");
    let first_command = format!("cat >/dev/null; touch {}", first_marker.display());
    let second_command = format!("cat >/dev/null; touch {}", second_marker.display());

    // The user approved the original command set...
    let first_engine = build_engine(workspace, &session_start_config(&[&first_command]), true);
    first_engine.approve_workspace_hooks().await;
    first_engine.run_session_start().await.expect("run session start");
    assert!(first_marker.exists());

    // ...then a repository update replaced the command (new digest). The
    // rebuilt engine must not inherit the old approval.
    let second_engine = build_engine(workspace, &session_start_config(&[&second_command]), true);
    assert!(second_engine.workspace_hooks_need_approval().await);
    second_engine.run_session_start().await.expect("run session start");
    assert!(
        !second_marker.exists(),
        "a command changed after approval must not execute under the stale approval"
    );
}

#[tokio::test]
async fn command_previews_list_the_exact_approved_set() {
    let temp_dir = TempDir::new().expect("tempdir");
    let workspace = temp_dir.path();

    let config = session_start_config(&["echo alpha", "echo beta"]);
    let engine = build_engine(workspace, &config, true);

    let previews = engine.command_previews();
    assert_eq!(previews.len(), 2);
    assert!(previews.iter().all(|preview| preview.event == "session_start"));
    let mut commands: Vec<&str> = previews.iter().map(|preview| preview.command.as_str()).collect();
    commands.sort();
    assert_eq!(commands, vec!["echo alpha", "echo beta"]);
}

#[tokio::test]
async fn workspace_sourced_agent_with_hooks_contributes_to_gate() {
    let mut spec = SubagentSpec {
        name: "plan".to_string(),
        description: "planning agent".to_string(),
        hooks: Some(session_start_config(&["echo agent-hook"])),
        ..Default::default()
    };

    for (source, expected) in [
        (SubagentSource::ProjectVtcode, true),
        (SubagentSource::ProjectClaude, true),
        (SubagentSource::ProjectCodex, true),
        (SubagentSource::UserVtcode, false),
        (SubagentSource::UserClaude, false),
        (SubagentSource::Builtin, false),
        (SubagentSource::Plugin { plugin: "p".to_string() }, false),
    ] {
        spec.source = source.clone();
        let agent = ActivePrimaryAgent::from_spec(&spec);
        assert_eq!(
            agent.contributes_workspace_controlled_hooks(),
            expected,
            "source {source:?} must gate workspace-sourced hooks"
        );
    }

    // A workspace-sourced agent WITHOUT hooks contributes nothing.
    spec.hooks = None;
    spec.source = SubagentSource::ProjectVtcode;
    let agent = ActivePrimaryAgent::from_spec(&spec);
    assert!(!agent.contributes_workspace_controlled_hooks());
}

#[tokio::test]
async fn persisted_approval_restores_only_when_digest_matches() {
    use crate::utils::dot_config::{
        initialize_dot_folder, load_lifecycle_hook_approval, update_lifecycle_hook_approval,
    };
    use vtcode_commons::env_lock::lock;

    let temp_dir = TempDir::new().expect("tempdir");
    let workspace = temp_dir.path();
    let marker = workspace.join("vt-restored-marker");
    let command = format!("cat >/dev/null; touch {}", marker.display());

    // Isolate the dot config used by the approval store.
    let env_guard = lock();
    let previous = std::env::var_os("VTCODE_CONFIG");
    env_guard.set_var("VTCODE_CONFIG", temp_dir.path().join("dot").to_string_lossy().into_owned());
    initialize_dot_folder().await.expect("init dot folder");

    // A gated engine whose exact command set was approved persists a record
    // keyed by workspace + digest; `restore_workspace_hook_approval` must
    // re-approve a rebuilt engine with the same command set...
    let first = build_engine(workspace, &session_start_config(&[&command]), true);
    let digest = first.command_digest().to_string();
    update_lifecycle_hook_approval(workspace, digest.clone())
        .await
        .expect("persist approval");
    assert_eq!(
        load_lifecycle_hook_approval(workspace)
            .await
            .expect("load approval")
            .expect("record")
            .config_digest,
        digest
    );

    let rebuilt = build_engine(workspace, &session_start_config(&[&command]), true);
    restore_workspace_hook_approval(&rebuilt, workspace).await;
    assert!(!rebuilt.workspace_hooks_need_approval().await);
    rebuilt.run_session_start().await.expect("run session start");
    assert!(marker.exists(), "matching persisted approval must restore the gate");

    // ...and must fail closed when the command set changed since approval.
    let changed_marker = workspace.join("vt-changed-marker");
    let changed_command = format!("touch {}", changed_marker.display());
    let changed = build_engine(workspace, &session_start_config(&[&changed_command]), true);
    restore_workspace_hook_approval(&changed, workspace).await;
    assert!(changed.workspace_hooks_need_approval().await);
    changed.run_session_start().await.expect("run session start");
    assert!(!changed_marker.exists(), "a changed command set must not inherit the persisted approval");

    env_guard.restore_var("VTCODE_CONFIG", previous);
}

#[tokio::test]
async fn carry_or_restore_keeps_session_only_approval_across_rebuild() {
    use vtcode_commons::env_lock::lock;

    let temp_dir = TempDir::new().expect("tempdir");
    let workspace = temp_dir.path();
    let marker = workspace.join("vt-carried-marker");
    let command = format!("cat >/dev/null; touch {}", marker.display());

    // Isolate the dot config so NO persisted record exists: only the in-memory
    // approval can survive the rebuild.
    let env_guard = lock();
    let previous = std::env::var_os("VTCODE_CONFIG");
    env_guard.set_var("VTCODE_CONFIG", temp_dir.path().join("dot").to_string_lossy().into_owned());

    let previous_engine = build_engine(workspace, &session_start_config(&[&command]), true);
    previous_engine.approve_workspace_hooks().await; // in-memory only (nothing persisted)

    let rebuilt = build_engine(workspace, &session_start_config(&[&command]), true);
    carry_or_restore_workspace_hook_approval(Some(&previous_engine), &rebuilt, workspace).await;
    assert!(!rebuilt.workspace_hooks_need_approval().await, "unchanged set must carry the approval");
    rebuilt.run_session_start().await.expect("run session start");
    assert!(marker.exists(), "carried approval must keep hooks running after a rebuild");

    env_guard.restore_var("VTCODE_CONFIG", previous);
}

#[tokio::test]
async fn carry_or_restore_fails_closed_on_changed_set_or_unapproved_previous() {
    use vtcode_commons::env_lock::lock;

    let temp_dir = TempDir::new().expect("tempdir");
    let workspace = temp_dir.path();
    let marker = workspace.join("vt-not-carried-marker");
    let command = format!("touch {}", marker.display());

    let env_guard = lock();
    let previous = std::env::var_os("VTCODE_CONFIG");
    env_guard.set_var("VTCODE_CONFIG", temp_dir.path().join("dot").to_string_lossy().into_owned());

    // Previous engine approved, but the rebuild changed the command set: the
    // approval must NOT carry and nothing is persisted, so the rebuild gates.
    let previous_engine = build_engine(workspace, &session_start_config(&["echo old"]), true);
    previous_engine.approve_workspace_hooks().await;

    let rebuilt = build_engine(workspace, &session_start_config(&[&command]), true);
    carry_or_restore_workspace_hook_approval(Some(&previous_engine), &rebuilt, workspace).await;
    assert!(rebuilt.workspace_hooks_need_approval().await, "changed set must not carry the approval");
    rebuilt.run_session_start().await.expect("run session start");
    assert!(!marker.exists(), "rebuild with a changed set must stay fail-closed");

    // Previous engine was gated but never approved: nothing carries.
    let unapproved_previous = build_engine(workspace, &session_start_config(&[&command]), true);
    let rebuilt_again = build_engine(workspace, &session_start_config(&[&command]), true);
    carry_or_restore_workspace_hook_approval(Some(&unapproved_previous), &rebuilt_again, workspace).await;
    assert!(rebuilt_again.workspace_hooks_need_approval().await);

    // Previous engine was ungated: nothing carries into a gated rebuild.
    let ungated_previous = build_engine(workspace, &session_start_config(&[&command]), false);
    let gated_rebuild = build_engine(workspace, &session_start_config(&[&command]), true);
    carry_or_restore_workspace_hook_approval(Some(&ungated_previous), &gated_rebuild, workspace).await;
    assert!(gated_rebuild.workspace_hooks_need_approval().await);

    env_guard.restore_var("VTCODE_CONFIG", previous);
}

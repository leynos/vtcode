//! Background restore ordering across a failed ACP archive attach.
//!
//! A resumed session defers restoring its background sub-agents until its
//! repaired history is durably checkpointed. A restored child can outlive a
//! failed attach, because the fail-closed path releases the workspace lease but
//! shutdown aborts only tracked children, never restored background children.
//!
//! Two properties keep a failed attach from starting work it cannot stop, and
//! from starting the same work twice:
//!
//! * a failed attach must not restore a background sub-agent before its
//!   required history checkpoint succeeds; and
//! * a failed owner migration must defer the workspace controller's restore,
//!   because the ownerless rows it would relaunch may yet be adopted by the
//!   session controller on a retry.
use super::archive::archived_listing;
use super::*;
use crate::zed::session::AcpSessionWorkspace;
use assert_fs::TempDir;
use diapason_core::llm::provider::ToolCall;
use diapason_core::subagents::TaskOwner;

/// A failed attach must not restore the background sub-agents it will own.
///
/// Restoring eagerly would launch a child before the required history
/// checkpoint. The fail-closed path releases the workspace lease, but shutdown
/// aborts only tracked children, never restored background children, so a
/// child launched before the failure would survive it. The persisted ledger is
/// the observable that proves no launch happened.
#[tokio::test]
async fn failed_attach_does_not_restore_background_subagents() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for the background-restore test");
    write_background_restore_fixture(temp.path());
    let session_id = acp::SessionId::new("background-restore-checkpoint-failure-session");
    let ledger_path = temp
        .path()
        .join(diapason_commons::workspace_dir_name())
        .join("state")
        .join("background_subagents.json");
    fs::create_dir_all(ledger_path.parent().expect("background ledger parent"))
        .expect("create the background state directory");
    let seeded_ledger = serde_json::json!({
        "records": [{
            "id": "background-background-demo",
            "agent_name": "background-demo",
            "display_label": "background-demo",
            "description": "background demo",
            "source": "builtin",
            "color": null,
            "owner_session_id": session_id.0.as_ref(),
            "session_id": "session-background-demo-restore",
            "exec_session_id": "exec-missing",
            "desired_enabled": true,
            "status": "stopped",
            "created_at": "2026-08-28T00:00:00Z",
            "updated_at": "2026-08-28T00:00:00Z",
            "started_at": null,
            "ended_at": "2026-08-28T00:00:00Z",
            "pid": null,
            "prompt": "restore me",
            "summary": null,
            "error": null,
            "archive_path": null,
            "transcript_path": null,
            "max_turns": null,
            "model_override": null,
            "reasoning_override": null,
            "restart_attempts": 0
        }]
    });
    let seeded_bytes = serde_json::to_vec(&seeded_ledger).expect("serialize the seeded background ledger");
    fs::write(&ledger_path, &seeded_bytes).expect("seed the background ledger");
    let mut vt_config = diapason_core::config::DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    let agent = build_agent_with_vt_config(temp.path(), "duck", Some(&vt_config)).await;
    let mut listing = archived_listing("background-restore-failure", temp.path(), "continue");
    listing.snapshot.messages = vec![SessionMessage::from(&Message::assistant_with_tools(
        String::new(),
        vec![ToolCall::function(
            "restore-call".to_string(),
            "apply_patch".to_string(),
            "{}".to_string(),
        )],
    ))];
    listing.snapshot.total_messages = 1;
    fs::create_dir(&listing.path).expect("occupy the archive path with a directory");

    let error = match agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            listing,
        )
        .await
    {
        Ok(_) => panic!("a directory at the archive path must make the required checkpoint fail"),
        Err(error) => error,
    };

    assert_eq!(
        error.to_string(),
        "Failed to persist repaired ACP tool-call history: archive_write_failed",
        "the deadline must fail at the required checkpoint"
    );
    let persisted_bytes = fs::read(&ledger_path).expect("read the background ledger after the failed attach");
    assert_eq!(
        persisted_bytes, seeded_bytes,
        "a failed attach must not restore a background sub-agent before its checkpoint succeeds"
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&persisted_bytes).expect("parse the ledger the failed attach left on disk");
    let record = &persisted["records"][0];
    assert_eq!(record["status"], "stopped", "a failed attach must leave the record stopped: {persisted}");
    assert_eq!(
        record["exec_session_id"], "exec-missing",
        "a failed attach must not replace the record's launch identity: {persisted}"
    );
    assert_eq!(
        record["owner_session_id"].as_str(),
        Some(session_id.0.as_ref()),
        "a failed attach must leave the seeded ownership untouched: {persisted}"
    );
    assert!(record["pid"].is_null(), "a restored record must not keep a live pid: {persisted}");
    assert_eq!(
        record["desired_enabled"], true,
        "a failed attach must not clear the record's desire to run: {persisted}"
    );
    assert!(
        agent
            .session_handle(&session_id)
            .expect("query the session registry after the failed attach")
            .is_none(),
        "a failed attach must not register the session"
    );
}

/// A failed owner migration must stop the workspace controller restoring the
/// ownerless rows the session controller may yet adopt.
///
/// The migration is made to fail deterministically by occupying the ledger's
/// sidecar lock path with a directory: every lock attempt then fails
/// immediately, while the controller's ledger load still succeeds. The session
/// workspace differs from the launch workspace so this attach must build its
/// own workspace runtime rather than reuse the launch one. The fixture script
/// writes a marker, which proves whether the workspace restore ran even though
/// the save that would record the launch cannot take the lock.
#[tokio::test]
async fn failed_background_owner_migration_defers_workspace_restore() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for the migration-failure test");
    let launch_workspace = temp.path();
    let session_workspace = launch_workspace.join("session-workspace");
    fs::create_dir_all(&session_workspace).expect("create the session workspace");
    let marker = session_workspace.join("background-restored-marker");
    write_background_restore_fixture_with_marker(&session_workspace, Some(&marker));
    let state_dir = session_workspace.join(diapason_commons::workspace_dir_name()).join("state");
    fs::create_dir_all(&state_dir).expect("create the background state directory");
    let ledger_path = state_dir.join("background_subagents.json");
    let seeded_ledger = serde_json::json!({
        "records": [{
            "id": "background-background-demo",
            "agent_name": "background-demo",
            "display_label": "background-demo",
            "description": "background demo",
            "source": "builtin",
            "color": null,
            "session_id": "session-background-demo-migration",
            "exec_session_id": "exec-missing",
            "desired_enabled": true,
            "status": "stopped",
            "created_at": "2026-08-28T00:00:00Z",
            "updated_at": "2026-08-28T00:00:00Z",
            "started_at": null,
            "ended_at": "2026-08-28T00:00:00Z",
            "pid": null,
            "prompt": "restore me",
            "summary": null,
            "error": null,
            "archive_path": null,
            "transcript_path": null,
            "max_turns": null,
            "model_override": null,
            "reasoning_override": null,
            "restart_attempts": 0
        }]
    });
    fs::write(&ledger_path, serde_json::to_vec(&seeded_ledger).expect("serialize the seeded background ledger"))
        .expect("seed the background ledger");
    fs::create_dir(state_dir.join("background_subagents.lock")).expect("occupy the ledger lock path with a directory");
    let mut vt_config = diapason_core::config::DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    let agent = build_agent_with_vt_config(launch_workspace, "duck", Some(&vt_config)).await;
    let session_id = acp::SessionId::new("background-migration-failure-session");
    let listing = archived_listing("background-migration-failure", &session_workspace, "continue");

    let handle = agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(&session_workspace)
                .await
                .expect("build the ACP session workspace from the session directory"),
            listing,
        )
        .await
        .expect("attach must continue when background owner migration fails");

    // The ledger cannot record the launch because the save fails, so the
    // controller's in-memory record is the only durable evidence of whether a
    // restore ran. A launch always replaces the exec session the ledger names,
    // and rollback settles the record without restoring that id, so this
    // assertion holds whether or not the spawned script managed to write the
    // marker before it was cancelled.
    let workspace_runtime = {
        let data = handle.data.lock().expect("lock the attached session data");
        data.workspace_runtime
            .clone()
            .expect("attach must retain its workspace runtime")
    };
    let controller = workspace_runtime
        .local_tool_registry
        .subagent_controller()
        .expect("the workspace runtime must retain its subagent controller");
    let entries = controller.background_status_entries(TaskOwner::Workspace).await;
    let record = entries
        .iter()
        .find(|entry| entry.id == "background-background-demo")
        .expect("the workspace controller must have loaded the seeded record");
    assert_eq!(
        record.exec_session_id, "exec-missing",
        "a deferred restore must leave the ownerless record unlaunched"
    );

    // The fixture script exits at once, so a spawned restore would write the
    // marker almost immediately. Poll briefly before asserting it never ran.
    for _ in 0..20 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        !marker.exists(),
        "a failed owner migration must stop the workspace controller restoring the ownerless row"
    );
}

/// Writes the `background-demo` spec and a script that exits at once.
///
/// The demo agent launches its script directly, so a restore attempt needs both
/// a discoverable spec and an executable. The script must not sleep: the
/// regression proves a failed attach never launches it, and it must leave no
/// child behind if it ever does.
fn write_background_restore_fixture(workspace: &Path) {
    write_background_restore_fixture_with_marker(workspace, None);
}

/// Writes the same fixture, optionally recording that the script ran.
///
/// A marker lets a regression observe an erroneous restore even when the save
/// that would record the launch fails; the ledger alone cannot show it.
fn write_background_restore_fixture_with_marker(workspace: &Path, marker: Option<&Path>) {
    let agent_dir = workspace.join(".diapason/agents");
    fs::create_dir_all(&agent_dir).expect("create background fixture agent directory");
    fs::write(
        agent_dir.join("background-demo.md"),
        r#"---
name: background-demo
description: Background restore checkpoint fixture.
tools:
  - command_session
background: true
maxTurns: 2
initialPrompt: Report readiness once.
---

Run the managed background fixture.
"#,
    )
    .expect("write background fixture agent");
    let scripts_dir = workspace.join("scripts");
    fs::create_dir_all(&scripts_dir).expect("create background fixture scripts directory");
    let script = scripts_dir.join("demo-background-subagent.sh");
    let body = match marker {
        Some(marker) => {
            format!("#!/bin/sh\nprintf 'background-restored\\n'\nprintf 'restored\\n' > \"{}\"\n", marker.display())
        }
        None => "#!/bin/sh\nprintf 'background-restored\\n'\n".to_string(),
    };
    fs::write(&script, body).expect("write background fixture script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script).expect("background fixture script metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("make background fixture script executable");
    }
}

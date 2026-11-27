//! Tests for background subprocess rows in the local-agent sidebar.
use super::*;
use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use diapason_core::config::DiapasonConfig;
use diapason_core::config::ReasoningEffortLevel;
use diapason_core::subagents::{
    BackgroundOutput, BackgroundOutputError, BackgroundOutputReader, BackgroundTaskRef, SubagentControllerConfig,
};
use diapason_core::tools::exec_session::ExecSessionManager;
use diapason_core::tools::registry::PtySessionManager;
use futures::future::BoxFuture;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn timestamp(second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, second)
        .single()
        .expect("valid timestamp")
}

fn background_entry(status: BackgroundSubprocessStatus, updated_at: DateTime<Utc>) -> BackgroundSubprocessEntry {
    BackgroundSubprocessEntry {
        id: "background-1".to_string(),
        owner_session_id: None,
        session_id: "session-1".to_string(),
        exec_session_id: String::new(),
        agent_name: "default".to_string(),
        display_label: "default".to_string(),
        description: "Default agent".to_string(),
        source: "builtin".to_string(),
        colour: None,
        status,
        desired_enabled: true,
        created_at: updated_at,
        updated_at,
        started_at: None,
        ended_at: None,
        pid: None,
        summary: None,
        error: None,
        archive_path: None,
        transcript_path: None,
    }
}

#[test]
fn background_row_visibility_uses_the_snapshot_entry() {
    let refreshed = background_entry(BackgroundSubprocessStatus::Running, timestamp(0));
    let snapshot = BackgroundSubprocessSnapshot {
        entry: background_entry(BackgroundSubprocessStatus::Stopped, timestamp(1)),
        preview: String::new(),
    };

    assert!(
        background_local_agent_row(refreshed, Some(&snapshot)).is_none(),
        "a row must not survive when the observation used for rendering says it stopped"
    );
}

#[test]
fn background_row_renders_metadata_from_the_snapshot_entry() {
    let refreshed = background_entry(BackgroundSubprocessStatus::Running, timestamp(0));
    let mut observed = background_entry(BackgroundSubprocessStatus::Running, timestamp(1));
    observed.summary = Some("Observed summary".to_string());
    observed.transcript_path = Some(PathBuf::from("/tmp/observed.jsonl"));
    let snapshot = BackgroundSubprocessSnapshot {
        entry: observed,
        preview: "observed preview".to_string(),
    };

    let (updated_at, row) = background_local_agent_row(refreshed, Some(&snapshot)).expect("row should be visible");

    assert_eq!(updated_at, timestamp(1));
    assert_eq!(row.status, "running");
    assert_eq!(row.summary.as_deref(), Some("Observed summary"));
    assert_eq!(row.preview, "observed preview");
    assert_eq!(row.transcript_path, Some(PathBuf::from("/tmp/observed.jsonl")));
}

#[test]
fn background_row_without_a_snapshot_uses_the_refreshed_entry_placeholder() {
    let refreshed = background_entry(BackgroundSubprocessStatus::Running, timestamp(0));

    let (updated_at, row) = background_local_agent_row(refreshed, None).expect("row should be visible");

    assert_eq!(updated_at, timestamp(0));
    assert_eq!(row.status, "running");
    assert_eq!(row.preview, "Subprocess is running; waiting for the next transcript update.");
    assert_eq!(row.summary.as_deref(), Some("Running; waiting for transcript output."));
}

#[test]
fn background_row_without_a_snapshot_still_applies_visibility() {
    let refreshed = background_entry(BackgroundSubprocessStatus::Stopped, timestamp(0));

    assert!(background_local_agent_row(refreshed, None).is_none());
}

/// Counts the preview reads the sidebar projection requests.
#[derive(Default)]
struct RecordingOutputReader {
    reads: AtomicUsize,
}

impl BackgroundOutputReader for RecordingOutputReader {
    fn read_output<'a>(
        &'a self,
        _task: &'a BackgroundTaskRef<'a>,
    ) -> BoxFuture<'a, Result<BackgroundOutput, BackgroundOutputError>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(BackgroundOutput::Archived("unused preview".to_string())) })
    }
}

/// Writes a stopped background record for the workspace-global `default` agent.
fn write_stopped_background_ledger(workspace: &std::path::Path) -> Result<()> {
    let state_dir = workspace.join(diapason_commons::workspace_dir_name()).join("state");
    std::fs::create_dir_all(&state_dir)?;
    let record = serde_json::json!({
        "id": "background-default",
        "owner_session_id": null,
        "agent_name": "default",
        "display_label": "default",
        "description": "Default agent",
        "source": "builtin",
        "color": null,
        "session_id": "session-default",
        "parent_session_id": "parent-session",
        "exec_session_id": "exec-session-default",
        "desired_enabled": true,
        "status": "stopped",
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
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
        state_dir.join("background_subagents.json"),
        serde_json::to_string(&serde_json::json!({ "records": [record] }))?,
    )?;
    Ok(())
}

/// Builds a controller whose background previews go through `output_reader`.
async fn controller_with_output_reader(
    workspace: &std::path::Path,
    output_reader: Arc<dyn BackgroundOutputReader>,
) -> Result<SubagentController> {
    let vt_cfg = DiapasonConfig::default();
    let pty_sessions = PtySessionManager::new(workspace.to_path_buf(), vt_cfg.pty.clone());
    let exec_sessions = ExecSessionManager::new(workspace.to_path_buf(), pty_sessions.clone());
    SubagentController::new(SubagentControllerConfig {
        workspace_root: workspace.to_path_buf(),
        parent_session_id: "parent-session".to_string(),
        parent_model: "test-model".to_string(),
        parent_provider: "test-provider".to_string(),
        parent_reasoning_effort: ReasoningEffortLevel::Medium,
        api_key: String::new(),
        vt_cfg,
        openai_chatgpt_auth: None,
        depth: 0,
        workspace_gated: false,
        exec_sessions,
        output_reader: Some(output_reader),
        pty_manager: pty_sessions.manager().clone(),
        managed_background_runtime: false,
        lock_scope: None,
    })
    .await
}

/// A hidden background record must not cost a preview read.
///
/// The projection calls `background_snapshot` to build each visible row, and
/// that snapshot reads the preview through the output reader. A stopped record
/// is discarded by the projector, so reading its preview is pure waste: the
/// loop filters hidden entries first. The projector keeps its own
/// snapshot-based check, so a candidate that stops between observations is
/// still hidden.
#[tokio::test]
async fn a_stopped_background_record_is_skipped_before_its_preview_is_read() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path();
    write_stopped_background_ledger(workspace)?;
    let reader = Arc::new(RecordingOutputReader::default());
    let controller = Arc::new(controller_with_output_reader(workspace, reader.clone()).await?);
    let stopped = controller
        .background_status_entries(TaskOwner::Workspace)
        .await
        .into_iter()
        .find(|entry| entry.id == "background-default")
        .ok_or_else(|| anyhow::anyhow!("the fixture ledger must load its stopped record"))?;
    assert!(
        !background_local_agent_is_visible(&stopped),
        "the fixture record must fail the visibility predicate"
    );

    let entries = build_local_agent_entries(&controller, Vec::new(), vec![stopped]).await;

    assert!(entries.is_empty(), "a hidden record must not produce a sidebar row");
    assert_eq!(reader.reads.load(Ordering::SeqCst), 0, "a hidden record must not trigger a preview read");
    Ok(())
}

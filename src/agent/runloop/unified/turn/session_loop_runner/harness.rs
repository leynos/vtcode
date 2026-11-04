use anyhow::{Context, Result};
use std::path::Path;

use diapason_config::loader::DiapasonConfig;
use diapason_core::core::agent::features::FeatureSet;
use diapason_core::exec::events::{ThreadEvent, ThreadStartedEvent};
use diapason_memory::RetentionPolicy;
use tokio::task::spawn_blocking;

use crate::agent::runloop::unified::inline_events::harness::{
    HARNESS_LOG_MAX_AGE_DAYS, HarnessEventEmitter, prune_old_harness_logs, resolve_event_log_path,
};
use crate::agent::runloop::unified::run_loop_context::TurnRunId;

pub(super) async fn initialize_harness(
    workspace: &Path,
    vt_cfg: Option<&DiapasonConfig>,
    model: &str,
    turn_run_id: &TurnRunId,
) -> Result<Option<HarnessEventEmitter>> {
    let harness_config = vt_cfg.map(|cfg| cfg.agent.harness.clone()).unwrap_or_default();
    let legacy_path = harness_config
        .event_log_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .map(|path| resolve_event_log_path(path, turn_run_id));

    if let Some(log_path) = legacy_path.as_ref() {
        let log_dir = if log_path.extension().is_some() {
            log_path.parent().unwrap_or(Path::new(".")).to_path_buf()
        } else {
            log_path.clone()
        };
        let prune_result = spawn_blocking(move || {
            if log_dir.is_dir() {
                prune_old_harness_logs(&log_dir, HARNESS_LOG_MAX_AGE_DAYS)
            } else {
                Ok(())
            }
        })
        .await
        .context("harness retention task failed")?;
        if let Err(error) = prune_result {
            tracing::warn!(target: "diapason.harness", phase = "legacy_prune", path = %log_path.display(), error = %error, "legacy harness log pruning failed");
        }
    }

    let retention_workspace = workspace.to_path_buf();
    let retention_session_id = turn_run_id.0.clone();
    match spawn_blocking(move || {
        diapason_memory::apply_retention_preserving(
            &retention_workspace,
            RetentionPolicy::default(),
            Some(retention_session_id.as_str()),
        )
    })
    .await
    {
        Ok(Ok(removed)) if removed > 0 => {
            tracing::debug!(target: "diapason.harness", removed, "pruned closed canonical sessions");
        }
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(target: "diapason.harness", phase = "canonical_retention", error = %error, "canonical session retention failed")
        }
        Err(error) => {
            tracing::warn!(target: "diapason.harness", phase = "canonical_retention", error = %error, "canonical session retention task failed")
        }
    }

    // Canonical persistence is mandatory. Setup errors propagate instead of
    // silently downgrading the run to an unpersisted session.
    let emitter = HarnessEventEmitter::new_async(workspace, &turn_run_id.0, legacy_path).await?;

    let session_derived = diapason_memory::session_directory(workspace, &turn_run_id.0)?.join("derived");
    let features = FeatureSet::from_config(vt_cfg);
    if features.open_responses.emit_events {
        let open_responses_config = vt_cfg.map(|cfg| cfg.agent.open_responses.clone()).unwrap_or_default();
        let output_path = session_derived.join("open-responses.jsonl");
        if let Err(error) = emitter
            .enable_open_responses_async(open_responses_config, model, Some(output_path.clone()))
            .await
        {
            tracing::warn!(target: "diapason.harness", phase = "open_responses_setup", path = %output_path.display(), error = %error, "Open Responses setup failed");
        }
    }

    if vt_cfg.is_some_and(|cfg| cfg.telemetry.atif_enabled) {
        let atif_path = session_derived.join("atif-trajectory.json");
        if let Err(error) = emitter.enable_atif(model, atif_path.clone()) {
            tracing::warn!(target: "diapason.harness", phase = "atif_setup", path = %atif_path.display(), error = %error, "ATIF setup failed");
        }
    }

    if let Err(error) = emitter
        .emit(ThreadEvent::ThreadStarted(ThreadStartedEvent { thread_id: turn_run_id.0.clone() }))
        .context("failed to emit canonical thread.started event")
    {
        emitter.finish_after_unexpected_exit().await;
        return Err(error);
    }
    Ok(Some(emitter))
}

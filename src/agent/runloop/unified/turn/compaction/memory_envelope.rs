//! Binary-specific memory envelope refresh. The pure envelope build/persist
//! logic lives in `diapason_core::compaction::memory_envelope`; this module only
//! adds the binary runloop's `SessionStats`-backed refresh path.

use super::*;
use crate::agent::runloop::unified::state::SessionStats;

pub(crate) fn refresh_session_memory_envelope(
    workspace_root: &Path,
    session_id: &str,
    vt_cfg: Option<&DiapasonConfig>,
    history: &[Message],
    session_stats: &SessionStats,
    envelope_update: Option<&SessionMemoryEnvelopeUpdate>,
) -> Result<Option<SessionMemoryEnvelope>> {
    if history.is_empty() || !should_persist_memory_envelope(vt_cfg) {
        return Ok(None);
    }

    let prior = load_latest_memory_envelope(workspace_root, session_id);
    let task_snapshot = read_task_tracker_snapshot(workspace_root);
    let touched_files = session_stats.recent_touched_files();
    let envelope = build_session_memory_envelope(
        session_id,
        workspace_root,
        history,
        &touched_files,
        derive_continuity_summary(history, prior.as_ref(), &task_snapshot),
        None,
        prior.as_ref(),
        &task_snapshot,
        envelope_update,
    );

    // Throttle: if nothing meaningful changed, keep the existing envelope and
    // avoid inserting another large system message into the history.
    if prior
        .as_ref()
        .is_some_and(|prior_envelope| prior_envelope.is_content_equivalent_to(&envelope))
    {
        return Ok(None);
    }

    let path = match latest_memory_envelope_path_for_session(workspace_root, session_id) {
        Some(path) => path,
        None => default_memory_envelope_path_for_session(workspace_root, session_id)?,
    };
    write_memory_envelope_to_path(&path, &envelope)?;
    Ok(Some(envelope))
}

/// Async post-turn checkpoint used by the interactive loop. The synchronous
/// wrapper above remains available to tests and legacy synchronous callers.
pub(crate) async fn refresh_session_memory_envelope_async(
    workspace_root: &Path,
    session_id: &str,
    vt_cfg: Option<&DiapasonConfig>,
    history: &[Message],
    session_stats: &SessionStats,
    envelope_update: Option<&SessionMemoryEnvelopeUpdate>,
) -> Result<Option<SessionMemoryEnvelope>> {
    if history.is_empty() || !should_persist_memory_envelope(vt_cfg) {
        return Ok(None);
    }

    let prior = load_latest_memory_envelope_async(workspace_root, session_id).await;
    let task_snapshot = read_task_tracker_snapshot_async(workspace_root).await;
    let touched_files = session_stats.recent_touched_files();
    let envelope = build_session_memory_envelope(
        session_id,
        workspace_root,
        history,
        &touched_files,
        derive_continuity_summary(history, prior.as_ref(), &task_snapshot),
        None,
        prior.as_ref(),
        &task_snapshot,
        envelope_update,
    );

    if prior
        .as_ref()
        .is_some_and(|prior_envelope| prior_envelope.is_content_equivalent_to(&envelope))
    {
        return Ok(None);
    }

    let path = match latest_memory_envelope_path_for_session_async(workspace_root, session_id).await {
        Some(path) => path,
        None => default_memory_envelope_path_for_session(workspace_root, session_id)?,
    };
    write_memory_envelope_to_path_async(&path, &envelope).await?;
    Ok(Some(envelope))
}

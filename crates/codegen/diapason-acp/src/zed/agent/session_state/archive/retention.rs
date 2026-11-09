//! Required-checkpoint retention.
//!
//! A required checkpoint must prove that the archive it just persisted still
//! carries the staged tool batch, so compaction cannot silently drop the
//! write-ahead evidence between staging and execution.

use anyhow::Context;
use diapason_commons::fs::read_private_json_file;
use diapason_core::llm::provider::{Message, MessageContent, MessageRole};
use diapason_core::utils::session_archive::{SessionMessage, SessionSnapshot};
use futures::future::BoxFuture;
use std::collections::HashSet;
use std::path::Path;

use super::CheckpointDurability;
use super::checkpoint::{CheckpointFailure, RequiredCheckpointError};

/// Returns the tool-call ids staged by the most recent assistant tool batch.
///
/// A required checkpoint must retain this batch; an empty result means the
/// snapshot carries no staged calls and needs no read-back verification.
pub(in crate::zed::agent::session_state) fn staged_tool_call_ids(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .rev()
        .find(|message| {
            message.role == MessageRole::Assistant
                && message.tool_calls.as_deref().is_some_and(|calls| !calls.is_empty())
        })
        .and_then(|message| message.tool_calls.as_deref())
        .map(|calls| calls.iter().map(|call| call.id.clone()).collect())
        .unwrap_or_default()
}

/// Returns the staged tool-call ids the persisted messages no longer retain.
///
/// A retained call must be requested and then answered in that order. Merely
/// finding its id somewhere in a request and somewhere in a result is not
/// enough: a truncated history can begin with a late result whose request was
/// dropped, and a request for a reused id can appear after a result that
/// answers an earlier batch. Compaction that drops either side, or reorders
/// them, has lost the staged batch.
pub(in crate::zed::agent::session_state) fn missing_persisted_tool_calls(
    staged_call_ids: &[String],
    persisted_messages: &[SessionMessage],
) -> Vec<String> {
    let mut requested = HashSet::new();
    let mut retained = HashSet::new();
    for message in persisted_messages {
        if let Some(calls) = message.tool_calls.as_deref() {
            requested.extend(calls.iter().map(|call| call.id.as_str()));
        }
        if message.role == MessageRole::Tool
            && let Some(call_id) = message.tool_call_id.as_deref()
            && requested.contains(call_id.as_str())
        {
            let _ = retained.insert(call_id.as_str());
        }
    }
    staged_call_ids
        .iter()
        .filter(|call_id| !retained.contains(call_id.as_str()))
        .cloned()
        .collect()
}

/// Returns the repaired calls whose exact persisted evidence is gone.
///
/// The per-id check accepts a request for a call id followed by any result for
/// it, so a later completed batch that reuses the id satisfies that check after
/// compaction drops the repaired placeholder or relocated result. Matching the
/// content alone is not enough either: a later result for the reused id can
/// carry identical text, or text that starts with the repaired result's prefix.
///
/// Provenance is positional instead. Compaction only removes messages from the
/// front of the snapshot, so the persisted payload is always a suffix of the
/// uncompacted history and each repaired message has exactly one persisted slot
/// that can still carry it. Retention therefore requires that slot to hold the
/// repaired message itself. A later message that reuses the call id occupies a
/// different slot, so it cannot stand in for dropped evidence.
pub(in crate::zed::agent::session_state) fn missing_persisted_evidence(
    required_evidence: &[SessionMessage],
    history_messages: &[SessionMessage],
    persisted_messages: &[SessionMessage],
) -> Vec<String> {
    let removed = history_messages.len().saturating_sub(persisted_messages.len());
    let mut search_start = 0;
    let mut missing = Vec::new();
    for evidence in required_evidence {
        let Some(history_index) = find_evidence_position(history_messages, evidence, search_start) else {
            missing.push(evidence_label(evidence));
            continue;
        };
        search_start = history_index + 1;
        let retained = history_index >= removed
            && persisted_messages
                .get(history_index - removed)
                .is_some_and(|persisted| evidence_slot_matches(evidence, persisted));
        if !retained {
            missing.push(evidence_label(evidence));
        }
    }
    missing
}

/// Returns the first position at or after `start` holding the repaired message.
fn find_evidence_position(
    history_messages: &[SessionMessage],
    evidence: &SessionMessage,
    start: usize,
) -> Option<usize> {
    history_messages
        .iter()
        .enumerate()
        .skip(start)
        .find(|(_, message)| evidence_slot_matches(evidence, message))
        .map(|(index, _)| index)
}

/// Returns the call id a missing-evidence report names.
fn evidence_label(evidence: &SessionMessage) -> String {
    evidence
        .tool_call_id
        .as_deref()
        .map_or_else(|| "<unidentified>".to_string(), ToString::to_string)
}

/// Returns whether a persisted slot still carries the repaired message.
fn evidence_slot_matches(expected: &SessionMessage, persisted: &SessionMessage) -> bool {
    expected.role == persisted.role
        && expected.tool_call_id == persisted.tool_call_id
        && evidence_content_matches(&expected.content, &persisted.content)
}

/// Returns whether persisted content is the repaired evidence, possibly shrunk.
///
/// The size-bounded archive compactor truncates an overlong string to roughly
/// half its length and appends an ellipsis, so persisted text may be a prefix
/// of the expected text followed by `"..."`. Compaction never lengthens a
/// value, so a persisted value that is not such a truncation is different
/// content even if it shares a prefix with the expected value.
fn evidence_content_matches(expected: &MessageContent, persisted: &MessageContent) -> bool {
    match (expected, persisted) {
        (MessageContent::Text(expected), MessageContent::Text(persisted)) => {
            !expected.is_empty()
                && !persisted.is_empty()
                && (expected == persisted
                    || persisted
                        .strip_suffix("...")
                        .is_some_and(|prefix| !prefix.is_empty() && expected.starts_with(prefix)))
        }
        _ => expected == persisted,
    }
}

/// Returns the staged calls a persisted history no longer retains as one batch.
///
/// The per-id check treats a call as retained when any message requests it and
/// any message answers it. That is enough for repaired evidence from earlier
/// batches, but it cannot see a dropped staged batch whose ids an older
/// completed batch reused: the old request and result satisfy both sets.
///
/// The staged batch is the one the checkpoint just appended, so it is the last
/// assistant tool batch in the snapshot. This check therefore requires the last
/// persisted assistant tool batch to request every staged id, with a tool result
/// for each of those ids after that message. A dropped batch is detected even
/// when older batches reused some of its ids, because the older batch is then
/// the last one and does not request the full staged set. Only a byte-identical
/// duplicate batch is indistinguishable from a retained one; the archive
/// repair path rejects that ambiguity before a checkpoint is attempted.
pub(in crate::zed::agent::session_state) fn missing_staged_batch_calls(
    staged_call_ids: &[String],
    persisted_messages: &[SessionMessage],
) -> Vec<String> {
    if staged_call_ids.is_empty() {
        return Vec::new();
    }
    let Some((batch_index, batch)) = persisted_messages.iter().enumerate().rev().find(|(_, message)| {
        message.role == MessageRole::Assistant && message.tool_calls.as_deref().is_some_and(|calls| !calls.is_empty())
    }) else {
        return staged_call_ids.to_vec();
    };
    let requested = batch
        .tool_calls
        .as_deref()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|call| call.id.as_str())
        .collect::<HashSet<_>>();
    let answered = persisted_messages
        .get(batch_index + 1..)
        .unwrap_or_default()
        .iter()
        .filter_map(|message| message.tool_call_id.as_deref())
        .map(String::as_str)
        .collect::<HashSet<_>>();
    staged_call_ids
        .iter()
        .filter(|call_id| {
            let call_id = call_id.as_str();
            !requested.contains(call_id) || !answered.contains(call_id)
        })
        .cloned()
        .collect()
}

/// Returns how many staged calls a checkpoint failed to retain, if any.
///
/// Best-effort checkpoints tolerate compaction loss; required checkpoints
/// must reject a snapshot that dropped the staged batch.
pub(in crate::zed::agent::session_state) fn required_checkpoint_retention_violation(
    durability: CheckpointDurability,
    staged_call_ids: &[String],
    persisted_messages: &[SessionMessage],
) -> Option<usize> {
    if durability != CheckpointDurability::Required || staged_call_ids.is_empty() {
        return None;
    }
    let missing = missing_persisted_tool_calls(staged_call_ids, persisted_messages);
    (!missing.is_empty()).then_some(missing.len())
}

/// Returns how many repaired calls a checkpoint failed to retain evidence for.
///
/// `history_messages` is the uncompacted payload the checkpoint prepared, which
/// fixes the position each repaired message must occupy after front trimming.
pub(in crate::zed::agent::session_state) fn required_checkpoint_evidence_violation(
    durability: CheckpointDurability,
    required_evidence: &[SessionMessage],
    history_messages: &[SessionMessage],
    persisted_messages: &[SessionMessage],
) -> Option<usize> {
    if durability != CheckpointDurability::Required || required_evidence.is_empty() {
        return None;
    }
    let missing = missing_persisted_evidence(required_evidence, history_messages, persisted_messages);
    (!missing.is_empty()).then_some(missing.len())
}

/// Returns how many staged calls a checkpoint failed to retain as one batch.
pub(in crate::zed::agent::session_state) fn required_staged_batch_violation(
    durability: CheckpointDurability,
    staged_call_ids: &[String],
    persisted_messages: &[SessionMessage],
) -> Option<usize> {
    if durability != CheckpointDurability::Required || staged_call_ids.is_empty() {
        return None;
    }
    let missing = missing_staged_batch_calls(staged_call_ids, persisted_messages);
    (!missing.is_empty()).then_some(missing.len())
}

/// Reads the messages a checkpoint just persisted.
///
/// This is a narrow, read-only seam. Production uses
/// [`SessionArchiveCheckpointReader`]; tests supply readers whose storage
/// failure or read-only behaviour is observable at the checkpoint boundary
/// without touching the real session store. The method returns a boxed future
/// so the checkpoint boundary can accept either implementation without
/// monomorphising its async state machine.
pub(in crate::zed::agent::session_state) trait CheckpointReader: Sync {
    /// Reads back the messages stored at the path the checkpoint wrote.
    fn read_persisted_messages<'a>(
        &'a self,
        persisted_path: &'a Path,
    ) -> BoxFuture<'a, anyhow::Result<Vec<SessionMessage>>>;
}

/// Reads checkpoints from the durable session archive.
///
/// The reader never writes, and it reads the exact file the checkpoint
/// reported rather than resolving the session identifier again. The two can
/// differ when an archive was attached from a listing whose file stem is not
/// the session id, and verifying a different file would not prove that this
/// checkpoint's payload was written.
pub(in crate::zed::agent::session_state) struct SessionArchiveCheckpointReader;

impl CheckpointReader for SessionArchiveCheckpointReader {
    fn read_persisted_messages<'a>(
        &'a self,
        persisted_path: &'a Path,
    ) -> BoxFuture<'a, anyhow::Result<Vec<SessionMessage>>> {
        Box::pin(read_back_persisted_messages(persisted_path))
    }
}

/// Reads the persisted checkpoint back and rejects dropped evidence.
///
/// `staged_call_ids` is the batch the checkpoint staged, which must survive
/// together as one batch. `required_call_ids` names every call the caller made
/// durable in this checkpoint, including repaired evidence from older batches.
/// `required_evidence` carries the messages the repair inserted or relocated,
/// so retention is checked against their content and position rather than their
/// call id alone. `history_messages` is the uncompacted payload that fixes those
/// positions.
pub(in crate::zed::agent::session_state) async fn verify_required_checkpoint_retention(
    reader: &dyn CheckpointReader,
    persisted_path: &Path,
    staged_call_ids: &[String],
    required_call_ids: &[String],
    required_evidence: &[SessionMessage],
    history_messages: &[SessionMessage],
) -> anyhow::Result<()> {
    let persisted_messages = reader
        .read_persisted_messages(persisted_path)
        .await
        .map_err(|error| RequiredCheckpointError::new(CheckpointFailure::ReadBackFailed, error))?;
    if let Some(missing) =
        required_staged_batch_violation(CheckpointDurability::Required, staged_call_ids, &persisted_messages)
    {
        return Err(RequiredCheckpointError::new(
            CheckpointFailure::RetentionDropped,
            anyhow::anyhow!(
                "required ACP session checkpoint dropped {missing} staged tool call(s) from their batch during \
                 compaction"
            ),
        )
        .into());
    }
    if let Some(missing) =
        required_checkpoint_retention_violation(CheckpointDurability::Required, required_call_ids, &persisted_messages)
    {
        return Err(RequiredCheckpointError::new(
            CheckpointFailure::RetentionDropped,
            anyhow::anyhow!("required ACP session checkpoint dropped {missing} staged tool call(s) during compaction"),
        )
        .into());
    }
    if let Some(missing) = required_checkpoint_evidence_violation(
        CheckpointDurability::Required,
        required_evidence,
        history_messages,
        &persisted_messages,
    ) {
        return Err(RequiredCheckpointError::new(
            CheckpointFailure::RetentionDropped,
            anyhow::anyhow!(
                "required ACP session checkpoint dropped repaired evidence for {missing} tool call(s) during \
                 compaction"
            ),
        )
        .into());
    }
    Ok(())
}

/// Loads the messages of the exact archive file a required checkpoint wrote.
async fn read_back_persisted_messages(persisted_path: &Path) -> anyhow::Result<Vec<SessionMessage>> {
    let snapshot: SessionSnapshot = read_private_json_file(persisted_path)
        .await
        .context("reading back the required ACP session checkpoint file")?;
    Ok(snapshot.messages)
}

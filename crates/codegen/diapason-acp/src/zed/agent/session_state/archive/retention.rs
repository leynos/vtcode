//! Required-checkpoint retention.
//!
//! A required checkpoint must prove that the archive it just persisted still
//! carries the staged tool batch, so compaction cannot silently drop the
//! write-ahead evidence between staging and execution.

use anyhow::Context;
use diapason_commons::fs::read_private_json_file;
use diapason_core::llm::provider::{Message, MessageContent, MessageRole};
use diapason_core::utils::session_archive::{SessionMessage, SessionSnapshot};
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
/// The per-id check accepts a request for a call id followed by any result
/// for it, so a later completed batch that reuses the id satisfies that check
/// after compaction drops the repaired placeholder or relocated result. The
/// evidence must also carry the content the repair produced: a reused call id
/// normally answers with different tool output, and only a byte-identical
/// result is genuinely indistinguishable.
pub(in crate::zed::agent::session_state) fn missing_persisted_evidence(
    required_evidence: &[SessionMessage],
    persisted_messages: &[SessionMessage],
) -> Vec<String> {
    required_evidence
        .iter()
        .filter(|evidence| {
            let Some(call_id) = evidence.tool_call_id.as_deref() else {
                return true;
            };
            let call_id = call_id.as_str();
            let mut requested = false;
            for message in persisted_messages {
                if message
                    .tool_calls
                    .as_deref()
                    .is_some_and(|calls| calls.iter().any(|call| call.id.as_str() == call_id))
                {
                    requested = true;
                }
                if requested
                    && message.role == MessageRole::Tool
                    && message.tool_call_id.as_deref().is_some_and(|id| id.as_str() == call_id)
                    && evidence_content_matches(&evidence.content, &message.content)
                {
                    return false;
                }
            }
            true
        })
        .filter_map(|evidence| evidence.tool_call_id.as_deref().cloned())
        .collect()
}

/// Returns whether persisted content still carries the repaired evidence.
///
/// The size-bounded archive compactor truncates overlong strings to a prefix
/// and appends an ellipsis, so either side may be the other's prefix. Output
/// from a different tool invocation, which a later batch that reuses the call
/// id carries, normally shares no such prefix.
fn evidence_content_matches(expected: &MessageContent, persisted: &MessageContent) -> bool {
    match (expected, persisted) {
        (MessageContent::Text(expected), MessageContent::Text(persisted)) => {
            !expected.is_empty()
                && !persisted.is_empty()
                && (expected.starts_with(persisted.as_str()) || persisted.starts_with(expected.as_str()))
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
pub(in crate::zed::agent::session_state) fn required_checkpoint_evidence_violation(
    durability: CheckpointDurability,
    required_evidence: &[SessionMessage],
    persisted_messages: &[SessionMessage],
) -> Option<usize> {
    if durability != CheckpointDurability::Required || required_evidence.is_empty() {
        return None;
    }
    let missing = missing_persisted_evidence(required_evidence, persisted_messages);
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
/// without touching the real session store.
pub(in crate::zed::agent::session_state) trait CheckpointReader {
    /// Reads back the messages stored at the path the checkpoint wrote.
    async fn read_persisted_messages(&self, persisted_path: &Path) -> anyhow::Result<Vec<SessionMessage>>;
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
    async fn read_persisted_messages(&self, persisted_path: &Path) -> anyhow::Result<Vec<SessionMessage>> {
        read_back_persisted_messages(persisted_path).await
    }
}

/// Reads the persisted checkpoint back and rejects dropped evidence.
///
/// `staged_call_ids` is the batch the checkpoint staged, which must survive
/// together as one batch. `required_call_ids` names every call the caller made
/// durable in this checkpoint, including repaired evidence from older batches.
/// `required_evidence` carries the messages the repair inserted or relocated,
/// so retention is checked against their content rather than their call id
/// alone.
pub(in crate::zed::agent::session_state) async fn verify_required_checkpoint_retention<R: CheckpointReader>(
    reader: &R,
    persisted_path: &Path,
    staged_call_ids: &[String],
    required_call_ids: &[String],
    required_evidence: &[SessionMessage],
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
    if let Some(missing) =
        required_checkpoint_evidence_violation(CheckpointDurability::Required, required_evidence, &persisted_messages)
    {
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

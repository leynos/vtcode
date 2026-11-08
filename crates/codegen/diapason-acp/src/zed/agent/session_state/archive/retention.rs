//! Required-checkpoint retention.
//!
//! A required checkpoint must prove that the archive it just persisted still
//! carries the staged tool batch, so compaction cannot silently drop the
//! write-ahead evidence between staging and execution.

use anyhow::Context;
use diapason_commons::fs::read_private_json_file;
use diapason_core::llm::provider::{Message, MessageRole};
use diapason_core::utils::session_archive::{SessionMessage, SessionSnapshot, find_session_by_identifier};
use std::collections::HashSet;
use std::path::Path;

use super::CheckpointDurability;

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
/// A retained call must appear both as a requested call id and as the
/// `tool_call_id` of its result; compaction that drops either side has lost
/// the staged batch.
pub(in crate::zed::agent::session_state) fn missing_persisted_tool_calls(
    staged_call_ids: &[String],
    persisted_messages: &[SessionMessage],
) -> Vec<String> {
    let requested = persisted_messages
        .iter()
        .filter_map(|message| message.tool_calls.as_deref())
        .flatten()
        .map(|call| call.id.as_str())
        .collect::<HashSet<_>>();
    let answered = persisted_messages
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

/// Reads the messages a checkpoint just persisted.
///
/// This is a narrow, read-only seam. Production uses
/// [`SessionArchiveCheckpointReader`]; tests supply readers whose storage
/// failure or read-only behaviour is observable at the checkpoint boundary
/// without touching the real session store.
pub(in crate::zed::agent::session_state) trait CheckpointReader {
    /// Reads back the messages stored for this checkpoint.
    async fn read_persisted_messages(
        &self,
        session_identifier: &str,
        persisted_path: &Path,
    ) -> anyhow::Result<Vec<SessionMessage>>;
}

/// Reads checkpoints from the durable session archive.
///
/// The reader never writes: it resolves the archive through the session
/// listing API, falling back to the exact file the checkpoint reported.
pub(in crate::zed::agent::session_state) struct SessionArchiveCheckpointReader;

impl CheckpointReader for SessionArchiveCheckpointReader {
    async fn read_persisted_messages(
        &self,
        session_identifier: &str,
        persisted_path: &Path,
    ) -> anyhow::Result<Vec<SessionMessage>> {
        read_back_persisted_messages(session_identifier, persisted_path).await
    }
}

/// Reads the persisted checkpoint back and rejects a dropped staged batch.
pub(in crate::zed::agent::session_state) async fn verify_required_checkpoint_retention<R: CheckpointReader>(
    reader: &R,
    session_identifier: &str,
    persisted_path: &Path,
    staged_call_ids: &[String],
) -> anyhow::Result<()> {
    let persisted_messages = reader.read_persisted_messages(session_identifier, persisted_path).await?;
    if let Some(missing) =
        required_checkpoint_retention_violation(CheckpointDurability::Required, staged_call_ids, &persisted_messages)
    {
        return Err(anyhow::anyhow!(
            "required ACP session checkpoint dropped {missing} staged tool call(s) during compaction"
        ));
    }
    Ok(())
}

/// Loads the messages of the archive a required checkpoint just persisted.
///
/// Canonical archives resolve through the session listing API. Archives
/// attached from an explicit listing path (for example a resumed archive
/// outside the session directory) are read from the exact file the checkpoint
/// reported, so the retention proof still covers what was written.
async fn read_back_persisted_messages(
    session_identifier: &str,
    persisted_path: &Path,
) -> anyhow::Result<Vec<SessionMessage>> {
    if let Some(listing) = find_session_by_identifier(session_identifier)
        .await
        .context("reading back the required ACP session checkpoint")?
    {
        return Ok(listing.snapshot.messages);
    }
    let persisted_stem = persisted_path.file_stem().and_then(|stem| stem.to_str());
    if persisted_stem == Some(session_identifier) {
        return Err(anyhow::anyhow!("required ACP session checkpoint could not be read back after persistence"));
    }
    let snapshot: SessionSnapshot = read_private_json_file(persisted_path)
        .await
        .context("reading back the required ACP session checkpoint file")?;
    Ok(snapshot.messages)
}

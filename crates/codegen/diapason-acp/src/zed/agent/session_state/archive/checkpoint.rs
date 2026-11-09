//! Durable session checkpoints and the required-checkpoint retention gate.
//!
//! A required checkpoint must reach storage before its caller proceeds, and
//! compaction must not silently drop the calls the checkpoint promised to
//! retain.
use diapason_core::core::agent::snapshots::SnapshotTurnDiagnostics;
use diapason_core::llm::provider::MessageRole;
use diapason_core::utils::session_archive::{
    SessionArchive, SessionMessage, SessionProgressArgs, history_persistence_enabled,
};
use tracing::debug;

use crate::zed::agent::session_state::ZedAgent;
use crate::zed::agent::tool_recovery::RetainedEvidence;
use crate::zed::types::SessionHandle;

use super::{
    CheckpointDurability, CheckpointReader, SessionArchiveCheckpointReader, missing_persisted_tool_calls,
    required_checkpoint_evidence_violation, required_checkpoint_retention_violation, required_staged_batch_violation,
    staged_tool_call_ids, trace_storage_operation, verify_required_checkpoint_retention,
};

/// The bounded reason a required checkpoint failed.
///
/// Callers report these categories instead of the raw cause, so a client error
/// and its log line stay stable while the underlying archive error remains in
/// the debug-only error chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::zed::agent) enum CheckpointFailure {
    /// History persistence is disabled, so nothing was written.
    PersistenceDisabled,
    /// The snapshot could not be prepared or written to storage.
    WriteFailed,
    /// The persisted archive could not be read back for verification.
    ReadBackFailed,
    /// The compacted payload dropped evidence the checkpoint promised.
    RetentionDropped,
}

impl CheckpointFailure {
    /// Returns the stable label used in logs and client-visible errors.
    pub(in crate::zed::agent) const fn label(self) -> &'static str {
        match self {
            Self::PersistenceDisabled => "persistence_disabled",
            Self::WriteFailed => "archive_write_failed",
            Self::ReadBackFailed => "archive_read_back_failed",
            Self::RetentionDropped => "staged_batch_dropped",
        }
    }
}

/// A required-checkpoint failure that carries its bounded category.
#[derive(Debug)]
pub(in crate::zed::agent) struct RequiredCheckpointError {
    failure: CheckpointFailure,
    source: anyhow::Error,
}

impl RequiredCheckpointError {
    /// Wraps a cause with the category reported at the failure boundary.
    pub(in crate::zed::agent) fn new(failure: CheckpointFailure, source: impl Into<anyhow::Error>) -> Self {
        Self { failure, source: source.into() }
    }

    /// Returns the bounded failure category.
    pub(in crate::zed::agent) const fn failure(&self) -> CheckpointFailure {
        self.failure
    }
}

impl std::fmt::Display for RequiredCheckpointError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print the wrapped chain so the debug-only suppressed-error log keeps
        // the cause and every context frame attached to it.
        write!(formatter, "{:#}", self.source)
    }
}

impl std::error::Error for RequiredCheckpointError {}

/// Wraps a checkpoint cause as a write failure.
fn write_failure(source: impl Into<anyhow::Error>) -> anyhow::Error {
    RequiredCheckpointError::new(CheckpointFailure::WriteFailed, source).into()
}

/// Whether a written checkpoint still carries the write-ahead protected batch.
///
/// A best-effort checkpoint that was skipped, failed, or compacted the batch
/// away reports [`Self::Unproven`], so the caller keeps write-ahead protection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::zed::agent) enum WriteAheadRetention {
    /// The checkpoint did not prove the protected batch is still persisted.
    #[default]
    Unproven,
    /// The written archive carries every write-ahead protected call.
    Retained,
}

impl ZedAgent {
    /// Persists the session snapshot without turn diagnostics.
    ///
    /// This is the best-effort form used for ordinary transcript checkpoints.
    /// Callers that must not proceed without durable storage use
    /// [`Self::checkpoint_session_required`].
    pub(in crate::zed::agent) async fn checkpoint_session(&self, session: &SessionHandle) -> anyhow::Result<()> {
        self.checkpoint_session_with_turn_diagnostics(
            session,
            None,
            CheckpointDurability::BestEffort,
            &[],
            &SessionArchiveCheckpointReader,
        )
        .await
        .map(|_retention| ())
    }

    /// Persists terminal tool results and releases write-ahead protection.
    ///
    /// The caller has replaced the staged placeholders with terminal results,
    /// so the batch no longer represents an uncertain effect once the archive
    /// carries it. Protection is released only when the best-effort write
    /// succeeded and retained every protected call; a skipped or failed write
    /// keeps it, so a later checkpoint cannot replace the archive that still
    /// holds the batch.
    pub(in crate::zed::agent) async fn checkpoint_terminal_tool_results(
        &self,
        session: &SessionHandle,
    ) -> anyhow::Result<()> {
        let retention = self
            .checkpoint_session_with_turn_diagnostics(
                session,
                None,
                CheckpointDurability::BestEffort,
                &[],
                &SessionArchiveCheckpointReader,
            )
            .await?;
        if retention == WriteAheadRetention::Retained {
            session
                .data
                .lock()
                .map_err(|error| write_failure(anyhow::anyhow!("ACP session lock poisoned: {error}")))?
                .write_ahead_call_ids
                .clear();
        }
        Ok(())
    }

    /// Persists the session snapshot and fails unless it reached storage.
    ///
    /// Used for the write-ahead checkpoint that gates tool execution: a
    /// disabled or throttled write is an error, so no tool runs without a
    /// durable record of the staged calls.
    pub(in crate::zed::agent) async fn checkpoint_session_required(
        &self,
        session: &SessionHandle,
    ) -> anyhow::Result<()> {
        self.checkpoint_session_with_turn_diagnostics(
            session,
            None,
            CheckpointDurability::Required,
            &[],
            &SessionArchiveCheckpointReader,
        )
        .await
        .map(|_retention| ())
    }

    /// Persists the session snapshot, optionally carrying turn diagnostics.
    ///
    /// `required_evidence` names the repaired messages the caller made durable
    /// in this checkpoint beyond the latest staged batch, such as the request
    /// and results an archive repair just inserted or relocated. Their content
    /// and recorded history position, not only their call id, must survive
    /// compaction.
    ///
    /// `reader` supplies the read-back used to verify the persisted payload.
    /// Production always passes [`SessionArchiveCheckpointReader`]; tests
    /// inject a failing reader to observe how the checkpoint boundary reports a
    /// storage read failure.
    pub(in crate::zed::agent::session_state) async fn checkpoint_session_with_turn_diagnostics(
        &self,
        session: &SessionHandle,
        turn_diagnostics: Option<SnapshotTurnDiagnostics>,
        durability: CheckpointDurability,
        required_evidence: &[RetainedEvidence],
        reader: &dyn CheckpointReader,
    ) -> anyhow::Result<WriteAheadRetention> {
        trace_storage_operation(
            "checkpoint",
            self.checkpoint_session_inner(session, turn_diagnostics, durability, required_evidence, reader),
        )
        .await
    }

    /// Writes the checkpoint unless history persistence is disabled.
    async fn checkpoint_session_inner(
        &self,
        session: &SessionHandle,
        turn_diagnostics: Option<SnapshotTurnDiagnostics>,
        durability: CheckpointDurability,
        required_evidence: &[RetainedEvidence],
        reader: &dyn CheckpointReader,
    ) -> anyhow::Result<WriteAheadRetention> {
        if !history_persistence_enabled() {
            if durability == CheckpointDurability::Required {
                return Err(RequiredCheckpointError::new(
                    CheckpointFailure::PersistenceDisabled,
                    anyhow::anyhow!("ACP session checkpoint requires history persistence, which is disabled"),
                )
                .into());
            }
            debug!("Skipped ACP session checkpoint because history persistence is disabled");
            return Ok(WriteAheadRetention::Unproven);
        }

        let (existing_archive, snapshot, session_identifier) = {
            let data = session
                .data
                .lock()
                .map_err(|error| write_failure(anyhow::anyhow!("ACP session lock poisoned: {error}")))?;
            let snapshot = data.thread.snapshot();
            (data.archive.clone(), snapshot, data.session_id.0.to_string())
        };

        let metadata = snapshot
            .metadata
            .clone()
            .ok_or_else(|| write_failure(anyhow::anyhow!("ACP session is missing archive metadata")))?;
        let mut archive = match existing_archive {
            Some(archive) => archive,
            None => {
                let archive = SessionArchive::new_with_identifier(metadata.clone(), session_identifier.clone())
                    .await
                    .map_err(write_failure)?;
                let mut data = session
                    .data
                    .lock()
                    .map_err(|error| write_failure(anyhow::anyhow!("ACP session lock poisoned: {error}")))?;
                data.archive = Some(archive.clone());
                archive
            }
        };
        archive.replace_metadata(metadata);

        // A required checkpoint gates tool execution, so remember which calls
        // it must retain before compaction can silently drop them. The latest
        // staged batch is always required; repair callers also name any
        // evidence they just made durable.
        let staged_call_ids = if durability == CheckpointDurability::Required {
            staged_tool_call_ids(&snapshot.messages)
        } else {
            Vec::new()
        };
        let retained_call_ids = if durability == CheckpointDurability::Required {
            let mut ids = staged_call_ids.clone();
            ids.extend(
                required_evidence
                    .iter()
                    .filter_map(|evidence| evidence.message.tool_call_id.as_deref().cloned()),
            );
            ids.sort();
            ids.dedup();
            ids
        } else {
            Vec::new()
        };
        let messages = snapshot.messages.iter().map(SessionMessage::from).collect::<Vec<_>>();
        // Repaired evidence is verified positionally: compaction only removes
        // messages from the front, so this uncompacted payload fixes which
        // persisted slot each evidence message must still occupy. Copy it only
        // when a caller supplied evidence that needs the check.
        let history_messages = if required_evidence.is_empty() {
            Vec::new()
        } else {
            messages.clone()
        };
        let recent_start = messages.len().saturating_sub(20);
        let recent_messages = messages.iter().skip(recent_start).cloned().collect();
        let turn_number = snapshot
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .count();
        let prepared = archive
            .prepare_checkpoint_snapshot(SessionProgressArgs {
                total_messages: messages.len(),
                distinct_tools: Vec::new(),
                messages,
                recent_messages,
                turn_number,
                token_usage: None,
                max_context_tokens: None,
                loaded_skills: Some(snapshot.loaded_skills),
                turn_diagnostics,
            })
            .map_err(write_failure)?;
        let Some(prepared_snapshot) = prepared else {
            debug!(persistence_outcome = "disabled", "Processed ACP session checkpoint");
            if durability == CheckpointDurability::Required {
                return Err(RequiredCheckpointError::new(
                    CheckpointFailure::PersistenceDisabled,
                    anyhow::anyhow!("ACP session checkpoint was not persisted (outcome: disabled)"),
                )
                .into());
            }
            return Ok(WriteAheadRetention::Unproven);
        };

        // Ordinary best-effort checkpoints must not overwrite the last archive
        // that still holds the live write-ahead evidence. Skipping the write
        // keeps that file intact; the next required checkpoint replaces the
        // protected set. A write that keeps the whole batch is reported so the
        // terminal-results caller can release protection it no longer needs.
        let mut write_ahead_retention = WriteAheadRetention::Unproven;
        if durability == CheckpointDurability::BestEffort {
            let protected_call_ids = session
                .data
                .lock()
                .map_err(|error| write_failure(anyhow::anyhow!("ACP session lock poisoned: {error}")))?
                .write_ahead_call_ids
                .clone();
            let missing_protected = missing_persisted_tool_calls(&protected_call_ids, &prepared_snapshot.messages);
            if !protected_call_ids.is_empty() {
                if missing_protected.is_empty() {
                    write_ahead_retention = WriteAheadRetention::Retained;
                } else {
                    debug!(
                        protected_calls = protected_call_ids.len(),
                        "Skipped ACP session checkpoint that would drop write-ahead tool-call evidence"
                    );
                    return Ok(WriteAheadRetention::Unproven);
                }
            }
        }

        // A required checkpoint rejects a compacted payload before it replaces
        // the archive on disk. Verifying only after the write would leave the
        // dropped evidence persisted, so a later retry could resume from it.
        if durability == CheckpointDurability::Required
            && (!retained_call_ids.is_empty() || !required_evidence.is_empty())
        {
            if let Some(missing) = required_staged_batch_violation(
                CheckpointDurability::Required,
                &staged_call_ids,
                &prepared_snapshot.messages,
            ) {
                return Err(RequiredCheckpointError::new(
                    CheckpointFailure::RetentionDropped,
                    anyhow::anyhow!(
                        "required ACP session checkpoint dropped {missing} staged tool call(s) from their batch \
                         during compaction"
                    ),
                )
                .into());
            }
            if let Some(missing) = required_checkpoint_retention_violation(
                CheckpointDurability::Required,
                &retained_call_ids,
                &prepared_snapshot.messages,
            ) {
                return Err(RequiredCheckpointError::new(
                    CheckpointFailure::RetentionDropped,
                    anyhow::anyhow!(
                        "required ACP session checkpoint dropped {missing} staged tool call(s) during compaction"
                    ),
                )
                .into());
            }
            if let Some(missing) = required_checkpoint_evidence_violation(
                CheckpointDurability::Required,
                required_evidence,
                &history_messages,
                &prepared_snapshot.messages,
            ) {
                return Err(RequiredCheckpointError::new(
                    CheckpointFailure::RetentionDropped,
                    anyhow::anyhow!(
                        "required ACP session checkpoint dropped repaired evidence for {missing} tool call(s) \
                         during compaction"
                    ),
                )
                .into());
            }
        }

        let persisted_path = archive
            .write_prepared_checkpoint_async(prepared_snapshot)
            .await
            .map_err(write_failure)?;
        debug!(persistence_outcome = "persisted", "Processed ACP session checkpoint");
        if durability == CheckpointDurability::Required
            && (!retained_call_ids.is_empty() || !required_evidence.is_empty())
        {
            verify_required_checkpoint_retention(
                reader,
                &persisted_path,
                &staged_call_ids,
                &retained_call_ids,
                required_evidence,
                &history_messages,
            )
            .await?;
        }
        if durability == CheckpointDurability::Required {
            // Later best-effort checkpoints must not compact this evidence away
            // until a newer required checkpoint protects its own batch.
            session
                .data
                .lock()
                .map_err(|error| write_failure(anyhow::anyhow!("ACP session lock poisoned: {error}")))?
                .write_ahead_call_ids = staged_call_ids;
        }
        Ok(write_ahead_retention)
    }
}

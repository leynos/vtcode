//! Durable session checkpoints and the required-checkpoint retention gate.
//!
//! A required checkpoint must reach storage before its caller proceeds, and
//! compaction must not silently drop the calls the checkpoint promised to
//! retain.
use diapason_core::core::agent::snapshots::SnapshotTurnDiagnostics;
use diapason_core::llm::provider::MessageRole;
use diapason_core::utils::session_archive::{
    SessionArchive, SessionMessage, SessionProgressArgs, SessionProgressPersistenceStatus, history_persistence_enabled,
};
use tracing::debug;

use crate::zed::agent::session_state::ZedAgent;
use crate::zed::types::SessionHandle;

use super::{
    CheckpointDurability, SessionArchiveCheckpointReader, staged_tool_call_ids, trace_storage_operation,
    verify_required_checkpoint_retention,
};

impl ZedAgent {
    /// Persists the session snapshot without turn diagnostics.
    ///
    /// This is the best-effort form used for ordinary transcript checkpoints.
    /// Callers that must not proceed without durable storage use
    /// [`Self::checkpoint_session_required`].
    pub(in crate::zed::agent) async fn checkpoint_session(&self, session: &SessionHandle) -> anyhow::Result<()> {
        self.checkpoint_session_with_turn_diagnostics(session, None, CheckpointDurability::BestEffort, &[])
            .await
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
        self.checkpoint_session_with_turn_diagnostics(session, None, CheckpointDurability::Required, &[])
            .await
    }

    /// Persists the session snapshot, optionally carrying turn diagnostics.
    ///
    /// `required_call_ids` names evidence the caller made durable in this
    /// checkpoint beyond the latest staged batch, such as calls an archive
    /// repair just inserted or relocated.
    pub(super) async fn checkpoint_session_with_turn_diagnostics(
        &self,
        session: &SessionHandle,
        turn_diagnostics: Option<SnapshotTurnDiagnostics>,
        durability: CheckpointDurability,
        required_call_ids: &[String],
    ) -> anyhow::Result<()> {
        trace_storage_operation(
            "checkpoint",
            self.checkpoint_session_inner(session, turn_diagnostics, durability, required_call_ids),
        )
        .await
    }

    /// Writes the checkpoint unless history persistence is disabled.
    async fn checkpoint_session_inner(
        &self,
        session: &SessionHandle,
        turn_diagnostics: Option<SnapshotTurnDiagnostics>,
        durability: CheckpointDurability,
        required_call_ids: &[String],
    ) -> anyhow::Result<()> {
        if !history_persistence_enabled() {
            if durability == CheckpointDurability::Required {
                return Err(anyhow::anyhow!("ACP session checkpoint requires history persistence, which is disabled"));
            }
            debug!("Skipped ACP session checkpoint because history persistence is disabled");
            return Ok(());
        }

        let (existing_archive, snapshot, session_identifier) = {
            let data = session
                .data
                .lock()
                .map_err(|error| anyhow::anyhow!("ACP session lock poisoned: {error}"))?;
            let snapshot = data.thread.snapshot();
            (data.archive.clone(), snapshot, data.session_id.0.to_string())
        };

        let metadata = snapshot
            .metadata
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ACP session is missing archive metadata"))?;
        let mut archive = match existing_archive {
            Some(archive) => archive,
            None => {
                let archive = SessionArchive::new_with_identifier(metadata.clone(), session_identifier.clone()).await?;
                let mut data = session
                    .data
                    .lock()
                    .map_err(|error| anyhow::anyhow!("ACP session lock poisoned: {error}"))?;
                data.archive = Some(archive.clone());
                archive
            }
        };
        archive.replace_metadata(metadata);

        // A required checkpoint gates tool execution, so remember which calls
        // it must retain before compaction can silently drop them. The latest
        // staged batch is always required; repair callers also name any
        // evidence they just made durable.
        let retained_call_ids = if durability == CheckpointDurability::Required {
            let mut ids = staged_tool_call_ids(&snapshot.messages);
            ids.extend(required_call_ids.iter().cloned());
            ids.sort();
            ids.dedup();
            ids
        } else {
            Vec::new()
        };
        let messages = snapshot.messages.iter().map(SessionMessage::from).collect::<Vec<_>>();
        let recent_start = messages.len().saturating_sub(20);
        let recent_messages = messages.iter().skip(recent_start).cloned().collect();
        let turn_number = snapshot
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .count();
        let persistence_status = archive
            .persist_checkpoint_async(SessionProgressArgs {
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
            .await?;
        let (persistence_outcome, persisted_path) = match persistence_status {
            SessionProgressPersistenceStatus::Persisted(path) => ("persisted", Some(path)),
            SessionProgressPersistenceStatus::Throttled(_) => ("throttled", None),
            SessionProgressPersistenceStatus::Disabled(_) => ("disabled", None),
        };
        debug!(persistence_outcome, "Processed ACP session checkpoint");
        let Some(persisted_path) = persisted_path else {
            if durability == CheckpointDurability::Required {
                return Err(anyhow::anyhow!(
                    "ACP session checkpoint was not persisted (outcome: {persistence_outcome})"
                ));
            }
            return Ok(());
        };
        if durability == CheckpointDurability::Required && !retained_call_ids.is_empty() {
            verify_required_checkpoint_retention(
                &SessionArchiveCheckpointReader,
                &session_identifier,
                &persisted_path,
                &retained_call_ids,
            )
            .await?;
        }
        Ok(())
    }
}

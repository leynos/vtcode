//! Builds and persists finalized and in-progress session archive snapshots.
use super::super::{
    SessionMessage, SessionProgress, SessionProgressArgs, SessionProgressPersistenceStatus, SessionSnapshot,
    apply_session_retention_best_effort, clean_transcript_lines, history_persistence_enabled,
    prepare_snapshot_for_write, progress_transcript_from_recent_messages,
};
use super::SessionArchive;
use crate::config::constants::defaults;
use crate::core::agent::snapshots::SnapshotTurnDiagnostics;
use crate::telemetry::perf::PerfSpan;
use crate::utils::session_transcript_norm::normalize_distinct_tools_for_summary;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vtcode_commons::VtCodePaths;
use vtcode_commons::fs::write_private_json_file;

struct SessionSnapshotContent {
    total_messages: usize,
    distinct_tools: Vec<String>,
    transcript: Vec<String>,
    messages: Vec<SessionMessage>,
    progress: Option<SessionProgress>,
}

fn write_private_json_file_sync<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let contents =
        serde_json::to_vec_pretty(value).with_context(|| format!("failed to serialize {}", path.display()))?;
    VtCodePaths::write_private_file_atomic(path, &contents)
        .with_context(|| format!("failed to write private session archive {}", path.display()))
}

impl SessionArchive {
    fn build_snapshot(&self, content: SessionSnapshotContent) -> SessionSnapshot {
        use crate::utils::error_log_collector::drain_error_logs;

        SessionSnapshot {
            metadata: self.metadata.clone(),
            started_at: self.started_at,
            ended_at: Utc::now(),
            total_messages: content.total_messages,
            distinct_tools: content.distinct_tools,
            transcript: content.transcript,
            messages: content.messages,
            progress: content.progress.map(Box::new),
            error_logs: drain_error_logs(),
        }
    }

    fn build_progress_snapshot(&self, args: SessionProgressArgs) -> SessionSnapshot {
        let SessionProgressArgs {
            total_messages,
            distinct_tools,
            messages,
            recent_messages,
            turn_number,
            token_usage,
            max_context_tokens,
            loaded_skills,
            turn_diagnostics,
        } = args;

        let transcript = progress_transcript_from_recent_messages(&recent_messages);
        let distinct_tools = normalize_distinct_tools_for_summary(&distinct_tools);
        let tool_summaries = distinct_tools.clone();

        self.build_snapshot(SessionSnapshotContent {
            total_messages,
            distinct_tools,
            transcript,
            messages,
            progress: Some(SessionProgress {
                turn_number,
                recent_messages,
                tool_summaries,
                token_usage,
                max_context_tokens,
                loaded_skills: loaded_skills.unwrap_or_default(),
                turn_diagnostics,
            }),
        })
    }

    pub fn finalize(
        &self,
        transcript: Vec<String>,
        total_messages: usize,
        distinct_tools: Vec<String>,
        messages: Vec<SessionMessage>,
    ) -> Result<PathBuf> {
        self.finalize_with_diagnostics(transcript, total_messages, distinct_tools, messages, None)
    }

    /// Finalize the archive while retaining lightweight diagnostics for the
    /// last turn. The final snapshot still omits the heavy progress history.
    pub fn finalize_with_diagnostics(
        &self,
        transcript: Vec<String>,
        total_messages: usize,
        distinct_tools: Vec<String>,
        messages: Vec<SessionMessage>,
        turn_diagnostics: Option<SnapshotTurnDiagnostics>,
    ) -> Result<PathBuf> {
        let snapshot = self.build_snapshot(SessionSnapshotContent {
            total_messages,
            distinct_tools: normalize_distinct_tools_for_summary(&distinct_tools),
            transcript: clean_transcript_lines(&transcript),
            messages,
            progress: turn_diagnostics.map(|turn_diagnostics| SessionProgress {
                turn_diagnostics: Some(turn_diagnostics),
                ..SessionProgress::default()
            }),
        });

        let path = self.write_snapshot(snapshot)?;
        if let Some(parent) = path.parent() {
            apply_session_retention_best_effort(parent);
        }
        Ok(path)
    }

    pub fn persist_progress(&self, args: SessionProgressArgs) -> Result<PathBuf> {
        let mut perf = PerfSpan::new("vtcode.perf.session_progress_write_ms");
        perf.tag("mode", "sync");

        let snapshot = self.build_progress_snapshot(args);

        self.write_snapshot(snapshot)
    }

    /// Persist progress asynchronously and report whether this call wrote a
    /// new durable snapshot.
    ///
    /// A throttled checkpoint still returns the archive path for diagnostics,
    /// but it must not be treated as durable by callers that acknowledge
    /// state only after history has been written.
    pub async fn persist_progress_async_with_status(
        &self,
        args: SessionProgressArgs,
    ) -> Result<SessionProgressPersistenceStatus> {
        self.persist_progress_async_with_status_inner(args, false).await
    }

    async fn persist_progress_async_with_status_inner(
        &self,
        args: SessionProgressArgs,
        bypass_throttle: bool,
    ) -> Result<SessionProgressPersistenceStatus> {
        let mut perf = PerfSpan::new("vtcode.perf.session_progress_write_ms");
        perf.tag("mode", if bypass_throttle { "async_forced" } else { "async" });

        if !history_persistence_enabled() {
            return Ok(SessionProgressPersistenceStatus::Disabled(self.path.clone()));
        }
        if !bypass_throttle && !self.should_persist_progress(args.turn_number)? {
            return Ok(SessionProgressPersistenceStatus::Throttled(self.path.clone()));
        }

        let snapshot = self.build_progress_snapshot(args);
        let path = self.write_snapshot_async(snapshot).await?;
        Ok(SessionProgressPersistenceStatus::Persisted(path))
    }

    /// Persist progress immediately, bypassing the normal progress throttle.
    ///
    /// This is intended for terminal handoff checkpoints where the latest
    /// conversation must be durable before an external artifact is written.
    pub async fn persist_progress_async_with_status_forced(
        &self,
        args: SessionProgressArgs,
    ) -> Result<SessionProgressPersistenceStatus> {
        self.persist_progress_async_with_status_inner(args, true).await
    }

    /// Persist a durable checkpoint without applying the progress throttle.
    ///
    /// Callers use this at state boundaries where losing the most recent
    /// message would make a resumed session inconsistent. History persistence
    /// and payload-size policy still apply.
    pub async fn persist_checkpoint_async(
        &self,
        args: SessionProgressArgs,
    ) -> Result<SessionProgressPersistenceStatus> {
        self.persist_progress_async_with_status_forced(args).await
    }

    pub async fn persist_progress_async(&self, args: SessionProgressArgs) -> Result<PathBuf> {
        Ok(self.persist_progress_async_with_status(args).await?.path().to_path_buf())
    }

    fn write_snapshot(&self, snapshot: SessionSnapshot) -> Result<PathBuf> {
        let Some(snapshot) = prepare_snapshot_for_write(snapshot)? else {
            return Ok(self.path.clone());
        };

        write_private_json_file_sync(&self.path, &snapshot)?;
        Ok(self.path.clone())
    }

    async fn write_snapshot_async(&self, snapshot: SessionSnapshot) -> Result<PathBuf> {
        let Some(snapshot) = prepare_snapshot_for_write(snapshot)? else {
            return Ok(self.path.clone());
        };

        write_private_json_file(&self.path, &snapshot).await?;
        Ok(self.path.clone())
    }

    fn should_persist_progress(&self, turn_number: usize) -> Result<bool> {
        let min_interval = Duration::from_millis(defaults::DEFAULT_SESSION_PROGRESS_MIN_INTERVAL_MS);
        let min_turns = defaults::DEFAULT_SESSION_PROGRESS_MIN_TURN_DELTA;

        let mut throttle = self
            .progress_throttle
            .lock()
            .map_err(|err| anyhow::anyhow!("session progress throttle lock poisoned: {err}"))
            .context("Failed to evaluate session progress persistence throttle")?;
        if turn_number <= throttle.last_turn {
            return Ok(false);
        }
        if throttle.last_written.elapsed() < min_interval && turn_number.saturating_sub(throttle.last_turn) < min_turns
        {
            return Ok(false);
        }

        throttle.last_written = Instant::now();
        throttle.last_turn = turn_number;
        Ok(true)
    }
}

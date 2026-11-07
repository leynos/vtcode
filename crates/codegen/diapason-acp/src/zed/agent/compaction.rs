//! Compact ACP session history to fit provider context limits.
//!
//! The agent checks prompt size before provider requests and coordinates
//! automatic, provider-native, and hook-assisted compaction.

use super::super::types::SessionHandle;
use super::redaction::{identifier_correlation, session_correlation};
use super::{ZedAgent, log_suppressed_error, warn_hook_message};
use crate::zed::provider_runtime::{ProviderAdmissionError, ProviderRequestRuntime};
use anyhow::{Context, Result};
use diapason_core::compaction::auto::{AutoCompactionInput, AutoCompactionOutcome, auto_compact_messages};
use diapason_core::compaction::memory_envelope::{MemoryEnvelopePlacement, local_compaction_config};
use diapason_core::compaction::{CompactionStrategy, ManualCompactionOptions, manual_compaction_strategy};
use diapason_core::exec::events::{CompactionMode, CompactionTrigger};
use diapason_core::llm::provider::{LLMProvider, ToolDefinition};
use std::future::Future;
use std::sync::Arc;
use tokio::time::{Instant, sleep_until};
use tracing::{Instrument, info, warn};

mod failure;

mod plan;

mod report;
use failure::CompactionFailure;
pub(super) use plan::{
    CompactionAttempt, CompactionCommit, CompactionDecision, CompactionRun, CompactionSessionSnapshot,
    estimated_prompt_tokens,
};
#[cfg(test)]
pub(super) use plan::{admission_prompt_budget, should_compact};
use report::{AdoptedCompaction, CompactionAttemptResult, CompactionReport};

const COMPACTION_SUMMARY_MAX_TOKENS: u32 = 4_096;

pub(super) struct CompactionRequest<'a> {
    pub(super) session: &'a SessionHandle,
    pub(super) provider: &'a dyn LLMProvider,
    pub(super) runtime: &'a ProviderRequestRuntime,
    pub(super) model: &'a str,
    pub(super) tools: Option<&'a Arc<Vec<ToolDefinition>>>,
}

impl ZedAgent {
    /// Compact the session before a prompt when a budget requires it.
    ///
    /// Publishes the whole activity lifecycle over ACP, including a terminal
    /// update that reports why a failed or cancelled attempt did not compact.
    /// Returns whether the history was actually reduced.
    pub(super) async fn maybe_compact_session(&self, request: CompactionRequest<'_>) -> Result<bool> {
        let CompactionRequest { session, provider, runtime, model, tools } = request;
        let Some(vt_config) = self.vt_config.as_deref() else {
            return Ok(false);
        };

        let resolved_messages = self.resolved_messages(session)?;
        let prompt_tokens = estimated_prompt_tokens(&resolved_messages, tools);
        let Some(decision) = CompactionDecision::for_prompt(Some(vt_config), provider, model, prompt_tokens) else {
            return Ok(false);
        };

        let snapshot = CompactionSessionSnapshot::load(session)?;
        let acp_session_id = crate::acp::SessionId::new(snapshot.session_id.clone());
        // Logs and spans correlate on a bounded key rather than the raw id.
        let session_key = session_correlation(&snapshot.session_id);
        self.compaction_recorder.record_started();
        // The edge supplies the identity and clock so the activity projection
        // stays a pure function of explicit inputs.
        let activity = super::lody_activity::CompactionActivity::begin(
            &acp_session_id,
            prompt_tokens,
            uuid::Uuid::new_v4(),
            std::time::Instant::now(),
        );
        self.publish_compaction_update(
            &acp_session_id,
            activity.started_update(),
            &session_key,
            "publish_started",
            "serialize_started",
        )
        .await;
        let span = tracing::info_span!(
            "acp_lody_compaction",
            operation = "context_compaction",
            session_correlation = %session_key,
            provider_correlation = %identifier_correlation(provider.name()),
            provider_bytes = provider.name().len(),
            model_correlation = %identifier_correlation(model),
            model_bytes = model.len(),
            prompt_tokens,
            outcome = tracing::field::Empty,
            error_category = tracing::field::Empty,
            elapsed_ms = tracing::field::Empty,
        );
        let attempt = self
            .run_compaction_attempt(
                CompactionRun {
                    session,
                    provider,
                    runtime,
                    model,
                    vt_config,
                    decision,
                    snapshot,
                },
                tools,
            )
            .instrument(span.clone())
            .await;
        let finished_at = std::time::Instant::now();
        let failure_category = self.record_compaction_outcome(
            &span,
            &attempt,
            CompactionReport {
                provider: provider.name(),
                model,
                prompt_tokens,
                session_key: &session_key,
                elapsed_ms: activity.elapsed_millis(finished_at),
            },
        );
        self.publish_compaction_update(
            &acp_session_id,
            activity.finished_update(finished_at, attempt.compacted, attempt.used_tokens_after, failure_category),
            &session_key,
            "publish_finished",
            "serialize_finished",
        )
        .await;
        match attempt.failure {
            Some(error) => Err(error),
            None => Ok(attempt.compacted),
        }
    }

    /// Run one compaction attempt and report what it achieved.
    ///
    /// A failure is carried in the result rather than returned because
    /// [`Self::commit_compacted_history`] adopts the compacted history before
    /// it can tell whether that history fits the admission budget. A caller
    /// that only saw an error would report a committed history as uncompacted.
    async fn run_compaction_attempt(
        &self,
        run: CompactionRun<'_>,
        tools: Option<&Arc<Vec<ToolDefinition>>>,
    ) -> CompactionAttemptResult {
        // Bind the copyable inputs before `run` is consumed by the attempt.
        let session = run.session;
        let provider = run.provider;
        let model = run.model;
        let decision = run.decision;
        let original_len = run.snapshot.history.len();
        let attempt = match self.run_compaction(run).await {
            Ok(attempt) => attempt,
            Err(error) => return CompactionAttemptResult::failed(error),
        };
        let outcome = match verify_compaction_result(attempt.outcome, decision) {
            Ok(Some(outcome)) => outcome,
            Ok(None) => return CompactionAttemptResult::no_change(),
            Err(error) => return CompactionAttemptResult::failed(error),
        };
        let adopted = match self
            .commit_compacted_history(CompactionCommit {
                session,
                thread: attempt.thread,
                history: attempt.history,
                tools,
                decision,
            })
            .await
        {
            Ok(adopted) => adopted,
            Err(error) => return CompactionAttemptResult::failed(error),
        };
        info!(
            provider_correlation = %identifier_correlation(provider.name()),
            provider_bytes = provider.name().len(),
            model_correlation = %identifier_correlation(model),
            model_bytes = model.len(),
            prompt_tokens = decision.prompt_tokens,
            compacted_prompt_tokens = adopted.prompt_tokens,
            original_len,
            compacted_len = outcome.compacted_len,
            compaction_mode = outcome.mode.as_str(),
            configured_threshold = ?decision.configured_threshold,
            admission_budget = ?decision.admission_budget,
            "Applied automatic ACP conversation compaction"
        );
        if adopted.over_budget {
            return CompactionAttemptResult::compacted_over_budget(
                adopted.prompt_tokens,
                CompactionFailure::admission_budget(format!(
                    "ACP compaction reduced the prompt from an estimated {} to {} tokens, which still exceeds its safe provider admission budget",
                    decision.prompt_tokens, adopted.prompt_tokens
                ))
                .into_error(),
            );
        }
        CompactionAttemptResult::compacted(adopted.prompt_tokens)
    }

    /// Run one compaction attempt under the provider admission permit.
    ///
    /// Suppression is lifted before the attempt so tool-driven compaction is
    /// not blocked by an earlier suppression, and restored afterwards.
    async fn run_compaction(&self, run: CompactionRun<'_>) -> Result<CompactionAttempt> {
        let CompactionRun {
            session,
            provider,
            runtime,
            model,
            vt_config,
            decision,
            snapshot,
        } = run;
        let CompactionSessionSnapshot {
            thread,
            session_id,
            mut history,
            mut auto_compact_suppressed,
        } = snapshot;
        let original_len = history.len();

        run_pre_compact_hooks(session, provider, model, original_len).await?;

        let permit = runtime.acquire(&session.cancellation).await.map_err(|error| match error {
            ProviderAdmissionError::Cancelled => CompactionFailure::cancelled().into_error(),
            ProviderAdmissionError::Closed { .. } | ProviderAdmissionError::QueueTimeout { .. } => {
                CompactionFailure::provider_admission().into_error()
            }
        })?;
        let total_deadline = runtime
            .deadline_policy()
            .total_generation
            .map(|timeout| Instant::now() + timeout);
        let outcome_result = {
            let workspace_runtime = session
                .workspace_runtime()
                .context("reading ACP session workspace runtime for compaction")?;
            let workspace_root = workspace_runtime
                .as_ref()
                .map_or(self.config.workspace.as_path(), |runtime| runtime.workspace_root.as_path());
            let compaction = auto_compact_messages(
                AutoCompactionInput {
                    provider,
                    model,
                    session_id: &session_id,
                    workspace_root,
                    vt_cfg: Some(vt_config),
                    current_token_usage: decision.prompt_tokens,
                    touched_files: &[],
                    engine_cfg: local_compaction_config(Some(vt_config), false),
                    manual_options: ManualCompactionOptions {
                        max_output_tokens: Some(COMPACTION_SUMMARY_MAX_TOKENS),
                        ..ManualCompactionOptions::default()
                    },
                    placement: MemoryEnvelopePlacement::BeforeLastUserOrSummary,
                    prefire: None,
                    auto_compact_suppressed: &mut auto_compact_suppressed,
                    force_compaction: decision.force_compaction,
                    steering_update: None,
                },
                &mut history,
            );
            tokio::pin!(compaction);
            await_compaction(session, total_deadline, compaction).await
        };
        drop(permit);

        {
            let mut data = session
                .data
                .lock()
                .map_err(|error| anyhow::anyhow!("ACP session lock poisoned: {error}"))?;
            data.auto_compact_suppressed = auto_compact_suppressed;
        }
        let outcome = outcome_result?;

        Ok(CompactionAttempt { outcome, thread, history })
    }

    /// Adopt the compacted history and report its remaining prompt size.
    ///
    /// The history is replaced before the estimate is taken, so the caller
    /// must treat the returned [`AdoptedCompaction`] as committed even when
    /// `over_budget` is set: retrying would duplicate the work and sending the
    /// history would be rejected by the provider.
    async fn commit_compacted_history(&self, commit: CompactionCommit<'_>) -> Result<AdoptedCompaction> {
        let CompactionCommit { session, thread, history, tools, decision } = commit;
        thread.replace_messages(history);
        if let Err(error) = self.checkpoint_session(session).await {
            log_suppressed_error(&error, "session_checkpoint", "archive_write_failed");
            warn!(
                operation = "session_checkpoint",
                error_category = "archive_write_failed",
                "Failed to persist compacted ACP session checkpoint"
            );
        }
        let prompt_tokens = estimated_prompt_tokens(&self.resolved_messages(session)?, tools);
        Ok(AdoptedCompaction {
            prompt_tokens,
            over_budget: decision.is_over_admission_budget(prompt_tokens),
        })
    }
}

/// Run the session's pre-compaction hooks for an automatic attempt.
///
/// Hook messages are warnings rather than failures; only the hook call itself
/// can fail the attempt, and it does so under the pre-compact-hook category.
async fn run_pre_compact_hooks(
    session: &SessionHandle,
    provider: &dyn LLMProvider,
    model: &str,
    original_len: usize,
) -> Result<()> {
    let Some(hooks) = session
        .lifecycle_hooks()
        .map_err(|error| CompactionFailure::pre_compact_hook(&error).into_error())?
    else {
        return Ok(());
    };

    let compaction_mode = lifecycle_compaction_mode(manual_compaction_strategy(provider, model));
    let outcome = hooks
        .run_pre_compact(CompactionTrigger::Auto, compaction_mode, original_len, 0, None)
        .await
        .map_err(|error| CompactionFailure::pre_compact_hook(&error).into_error())?;
    for message in outcome.messages {
        warn_hook_message("PreCompact", &message);
    }
    Ok(())
}

/// Await a compaction attempt, mapping cancellation and timeout to their
/// failure categories and any other error to the provider category.
async fn await_compaction<F>(
    session: &SessionHandle,
    total_deadline: Option<Instant>,
    compaction: std::pin::Pin<&mut F>,
) -> Result<Option<AutoCompactionOutcome>>
where
    F: Future<Output = Result<Option<AutoCompactionOutcome>>>,
{
    if let Some(deadline) = total_deadline {
        tokio::select! {
            () = session.cancellation.cancelled() => Err(CompactionFailure::cancelled().into_error()),
            () = sleep_until(deadline) => Err(CompactionFailure::timeout().into_error()),
            result = compaction => result.map_err(|error| CompactionFailure::provider(&error).into_error()),
        }
    } else {
        tokio::select! {
            () = session.cancellation.cancelled() => Err(CompactionFailure::cancelled().into_error()),
            result = compaction => result.map_err(|error| CompactionFailure::provider(&error).into_error()),
        }
    }
}

/// Reject an attempt that left an over-budget prompt unchanged.
///
/// No outcome means the compactor found nothing to do, which is only harmless
/// while the prompt is still inside the admission budget.
fn verify_compaction_result(
    outcome: Option<AutoCompactionOutcome>,
    decision: CompactionDecision,
) -> Result<Option<AutoCompactionOutcome>> {
    let Some(outcome) = outcome else {
        warn!(
            prompt_tokens = decision.prompt_tokens,
            configured_threshold = ?decision.configured_threshold,
            admission_budget = ?decision.admission_budget,
            "ACP context pressure crossed its admission budget but compaction made no change"
        );
        if decision.is_over_admission_budget(decision.prompt_tokens) {
            return Err(CompactionFailure::admission_budget(format!(
                "ACP prompt uses an estimated {} tokens, exceeds its safe provider admission budget, and automatic compaction made no change",
                decision.prompt_tokens
            ))
            .into_error());
        }
        return Ok(None);
    };
    Ok(Some(outcome))
}

/// Report which engine lifecycle hooks should assume for a strategy.
fn lifecycle_compaction_mode(strategy: CompactionStrategy) -> CompactionMode {
    match strategy {
        CompactionStrategy::Local => CompactionMode::Local,
        CompactionStrategy::NativeStandalone | CompactionStrategy::NativeInline => CompactionMode::Provider,
    }
}

#[cfg(test)]
#[path = "compaction/tests.rs"]
mod tests;

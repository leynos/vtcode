//! Compact ACP session history to fit provider context limits.
//!
//! The agent checks prompt size before provider requests and coordinates
//! automatic, provider-native, and hook-assisted compaction.

use super::super::types::SessionHandle;
use super::{ZedAgent, log_suppressed_error, warn_hook_message};
use crate::zed::provider_runtime::{ProviderAdmissionError, ProviderRequestRuntime};
use anyhow::{Context, Result};
use std::future::Future;
use std::sync::Arc;
use tokio::time::{Instant, sleep_until};
use tracing::{Instrument, info, warn};
use vtcode_core::compaction::auto::{AutoCompactionInput, AutoCompactionOutcome, auto_compact_messages};
use vtcode_core::compaction::memory_envelope::{MemoryEnvelopePlacement, local_compaction_config};
use vtcode_core::compaction::{CompactionStrategy, ManualCompactionOptions, manual_compaction_strategy};
use vtcode_core::exec::events::{CompactionMode, CompactionTrigger};
use vtcode_core::llm::provider::{LLMProvider, ToolDefinition};

mod failure;
mod outcome;
mod plan;
use failure::{CompactionFailure, classify};
use outcome::compaction_outcome_label;
pub(super) use plan::{
    CompactionAttempt, CompactionCommit, CompactionDecision, CompactionRun, CompactionSessionSnapshot,
    estimated_prompt_tokens,
};
#[cfg(test)]
pub(super) use plan::{admission_prompt_budget, should_compact};

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
        // The edge supplies the identity and clock so the activity projection
        // stays a pure function of explicit inputs.
        let activity = super::lody_activity::CompactionActivity::begin(
            &acp_session_id,
            prompt_tokens,
            uuid::Uuid::new_v4(),
            std::time::Instant::now(),
        );
        match activity.started_update() {
            Ok(update) => {
                if let Err(error) = self.send_update(&acp_session_id, update).await {
                    log_activity_update_failure(&error, &acp_session_id, "publish_started");
                }
            }
            Err(error) => log_activity_update_failure(&error, &acp_session_id, "serialize_started"),
        }
        let span = tracing::info_span!(
            "acp_lody_compaction",
            operation = "context_compaction",
            session_id = %acp_session_id,
            provider = provider.name(),
            model,
            prompt_tokens,
            outcome = tracing::field::Empty,
            error_category = tracing::field::Empty,
            elapsed_ms = tracing::field::Empty,
        );
        let compaction_result: Result<bool> = async {
            let original_len = snapshot.history.len();
            let attempt = self
                .run_compaction(CompactionRun {
                    session,
                    provider,
                    runtime,
                    model,
                    vt_config,
                    decision,
                    snapshot,
                })
                .await?;
            let Some(outcome) = verify_compaction_result(attempt.outcome, decision)? else {
                return Ok(false);
            };

            let compacted_prompt_tokens = self
                .commit_compacted_history(CompactionCommit {
                    session,
                    thread: attempt.thread,
                    history: attempt.history,
                    tools,
                    decision,
                })
                .await?;
            info!(
                provider = provider.name(),
                model,
                prompt_tokens = decision.prompt_tokens,
                compacted_prompt_tokens,
                original_len,
                compacted_len = outcome.compacted_len,
                compaction_mode = outcome.mode.as_str(),
                configured_threshold = ?decision.configured_threshold,
                admission_budget = ?decision.admission_budget,
                "Applied automatic ACP conversation compaction"
            );
            Ok(true)
        }
        .instrument(span.clone())
        .await;
        // `Ok(false)` means the attempt produced no compaction outcome while
        // staying inside the admission budget, so report a completed but
        // non-compacting activity instead of a misleading token reduction.
        let compacted = matches!(compaction_result, Ok(true));
        let used_tokens_after = if compacted {
            self.resolved_messages(session)
                .ok()
                .map(|messages| estimated_prompt_tokens(&messages, tools))
        } else {
            None
        };
        // Clients only ever see this closed-set category; the originating
        // error is detailed through the redacted debug helper below.
        let failure_category = compaction_result.as_ref().err().map(|error| classify(error).category());
        let outcome = compaction_outcome_label(failure_category.is_some(), compacted);
        let finished_at = std::time::Instant::now();
        let elapsed_ms = activity.elapsed_millis(finished_at);
        let _ = span.record("outcome", outcome);
        let _ = span.record("elapsed_ms", elapsed_ms);
        if let (Some(error), Some(category)) = (compaction_result.as_ref().err(), failure_category) {
            let _ = span.record("error_category", category);
            log_suppressed_error(error, "context_compaction", category);
            warn!(
                provider = provider.name(),
                model,
                prompt_tokens,
                elapsed_ms,
                outcome,
                error_category = category,
                "ACP automatic context compaction failed"
            );
        } else {
            info!(
                provider = provider.name(),
                model, prompt_tokens, compacted, elapsed_ms, outcome, "ACP automatic context compaction finished"
            );
        }
        match activity.finished_update(finished_at, compacted, used_tokens_after, failure_category) {
            Ok(update) => {
                if let Err(error) = self.send_update(&acp_session_id, update).await {
                    log_activity_update_failure(&error, &acp_session_id, "publish_finished");
                }
            }
            Err(error) => log_activity_update_failure(&error, &acp_session_id, "serialize_finished"),
        }
        compaction_result
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
            other => CompactionFailure::provider_detail(format!("provider admission failed: {other}")).into_error(),
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
    /// Fails with an admission-budget category when the summary still exceeds
    /// the safe prompt budget, because retrying would duplicate the work and
    /// sending it would be rejected by the provider.
    async fn commit_compacted_history(&self, commit: CompactionCommit<'_>) -> Result<usize> {
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
        let compacted_prompt_tokens = estimated_prompt_tokens(&self.resolved_messages(session)?, tools);
        if decision.is_over_admission_budget(compacted_prompt_tokens) {
            return Err(CompactionFailure::admission_budget(format!(
                "ACP compaction reduced the prompt from an estimated {} to {compacted_prompt_tokens} tokens, which still exceeds its safe provider admission budget",
                decision.prompt_tokens
            ))
            .into_error());
        }
        Ok(compacted_prompt_tokens)
    }
}

/// Report a compaction activity update that could not be serialized or sent.
///
/// A failed update never fails the compaction itself, but the raw transport
/// or serialization error may embed workspace or archive detail, so only the
/// fixed category reaches the warning and the detail goes to the redacted
/// debug helper.
fn log_activity_update_failure(
    error: &impl std::fmt::Display,
    session_id: &crate::acp::SessionId,
    stage: &'static str,
) {
    log_suppressed_error(error, "context_compaction_update", "compaction_update_failed");
    warn!(
        session_id = %session_id,
        stage,
        operation = "context_compaction_update",
        error_category = "compaction_update_failed",
        "Failed to publish an ACP compaction activity update"
    );
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

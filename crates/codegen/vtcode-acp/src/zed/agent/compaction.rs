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
use tracing::{info, warn};
use vtcode_core::compaction::auto::{AutoCompactionInput, AutoCompactionOutcome, auto_compact_messages};
use vtcode_core::compaction::memory_envelope::{
    MemoryEnvelopePlacement, effective_compaction_threshold, local_compaction_config,
};
use vtcode_core::compaction::{CompactionStrategy, ManualCompactionOptions, manual_compaction_strategy};
use vtcode_core::exec::events::{CompactionMode, CompactionTrigger};
use vtcode_core::llm::provider::{LLMProvider, Message, ToolDefinition};
use vtcode_core::{config::VTCodeConfig, core::threads::ThreadRuntimeHandle};

const DEFAULT_OUTPUT_RESERVE_TOKENS: usize = 16_384;
const ADMISSION_SAFETY_MARGIN_TOKENS: usize = 1_024;
const COMPACTION_SUMMARY_MAX_TOKENS: u32 = 4_096;

fn estimated_tool_tokens(tools: Option<&Arc<Vec<ToolDefinition>>>) -> usize {
    tools
        .and_then(|definitions| serde_json::to_vec(definitions.as_ref()).ok())
        .map_or(0, |encoded| encoded.len().div_ceil(4))
}

fn estimated_prompt_tokens(messages: &[Message], tools: Option<&Arc<Vec<ToolDefinition>>>) -> usize {
    messages
        .iter()
        .map(Message::estimate_tokens)
        .sum::<usize>()
        .saturating_add(estimated_tool_tokens(tools))
}

fn admission_prompt_budget(provider: &dyn LLMProvider, model: &str) -> Option<usize> {
    let context_size = provider.effective_context_size(model);
    if context_size == 0 {
        return None;
    }

    let output_reserve = DEFAULT_OUTPUT_RESERVE_TOKENS.max(context_size / 10).min(context_size / 2);
    let safety_margin = ADMISSION_SAFETY_MARGIN_TOKENS.min(context_size / 10);
    Some(context_size.saturating_sub(output_reserve).saturating_sub(safety_margin).max(1))
}

#[cfg(test)]
fn should_compact(provider: &dyn LLMProvider, model: &str, prompt_tokens: usize) -> bool {
    let threshold = effective_compaction_threshold(None, provider, model);
    let admission_budget = admission_prompt_budget(provider, model);
    threshold
        .into_iter()
        .chain(admission_budget)
        .min()
        .is_some_and(|budget| prompt_tokens >= budget)
}

pub(super) struct CompactionRequest<'a> {
    pub(super) session: &'a SessionHandle,
    pub(super) provider: &'a dyn LLMProvider,
    pub(super) runtime: &'a ProviderRequestRuntime,
    pub(super) model: &'a str,
    pub(super) tools: Option<&'a Arc<Vec<ToolDefinition>>>,
}

/// Validate the candidate, including resolved prompt and tool overhead, before
/// the caller publishes live history or writes its checkpoint.
fn admit_compacted_history(
    history: &[Message],
    prompt_tokens: usize,
    original_history_tokens: usize,
    admission_budget: Option<usize>,
) -> Result<usize> {
    let compacted_prompt_tokens = prompt_tokens
        .saturating_sub(original_history_tokens)
        .saturating_add(history.iter().map(Message::estimate_tokens).sum::<usize>());
    if admission_budget.is_some_and(|budget| compacted_prompt_tokens >= budget) {
        anyhow::bail!(
            "ACP compaction reduced the prompt from an estimated {prompt_tokens} to {compacted_prompt_tokens} tokens, which still exceeds its safe provider admission budget"
        );
    }
    Ok(compacted_prompt_tokens)
}

impl ZedAgent {
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
        let activity = super::lody_activity::CompactionActivity::begin(&acp_session_id, prompt_tokens);
        match activity.started_update() {
            Ok(update) => {
                if let Err(error) = self.send_update(&acp_session_id, update).await {
                    warn!(%error, session_id = %acp_session_id, "Failed to publish ACP compaction start update");
                }
            }
            Err(error) => {
                warn!(%error, session_id = %acp_session_id, "Failed to serialize ACP compaction start update")
            }
        }
        let compaction_result: Result<bool> = async {
            let original_len = snapshot.history.len();
            let original_history_tokens = snapshot.history.iter().map(Message::estimate_tokens).sum();
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
                    decision,
                    original_history_tokens,
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
        .await;
        let used_tokens_after = compaction_result.as_ref().ok().and_then(|_| {
            self.resolved_messages(session)
                .ok()
                .map(|messages| estimated_prompt_tokens(&messages, tools))
        });
        let failure_reason = compaction_result.as_ref().err().map(ToString::to_string);
        match activity.finished_update(used_tokens_after, failure_reason.as_deref()) {
            Ok(update) => {
                if let Err(error) = self.send_update(&acp_session_id, update).await {
                    warn!(%error, session_id = %acp_session_id, "Failed to publish ACP compaction terminal update");
                }
            }
            Err(error) => {
                warn!(%error, session_id = %acp_session_id, "Failed to serialize ACP compaction terminal update")
            }
        }
        compaction_result
    }

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
            ProviderAdmissionError::Cancelled => anyhow::anyhow!("ACP compaction cancelled"),
            other => anyhow::anyhow!(other.to_string()),
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

    async fn commit_compacted_history(&self, commit: CompactionCommit<'_>) -> Result<usize> {
        let CompactionCommit {
            session,
            thread,
            history,
            decision,
            original_history_tokens,
        } = commit;
        let compacted_prompt_tokens = admit_compacted_history(
            &history,
            decision.prompt_tokens,
            original_history_tokens,
            decision.admission_budget,
        )?;
        thread.replace_messages(history);
        if let Err(error) = self.checkpoint_session(session).await {
            log_suppressed_error(&error, "session_checkpoint", "archive_write_failed");
            warn!(
                operation = "session_checkpoint",
                error_category = "archive_write_failed",
                "Failed to persist compacted ACP session checkpoint"
            );
        }
        Ok(compacted_prompt_tokens)
    }
}

#[derive(Clone, Copy)]
struct CompactionDecision {
    prompt_tokens: usize,
    configured_threshold: Option<usize>,
    admission_budget: Option<usize>,
    force_compaction: bool,
}

impl CompactionDecision {
    fn for_prompt(
        vt_config: Option<&VTCodeConfig>,
        provider: &dyn LLMProvider,
        model: &str,
        prompt_tokens: usize,
    ) -> Option<Self> {
        let configured_threshold = effective_compaction_threshold(vt_config, provider, model);
        let admission_budget = admission_prompt_budget(provider, model);
        let trigger = configured_threshold.into_iter().chain(admission_budget).min();
        if trigger.is_none_or(|budget| prompt_tokens < budget) {
            return None;
        }

        let force_compaction = admission_budget.is_some_and(|budget| prompt_tokens >= budget)
            && configured_threshold.is_none_or(|threshold| prompt_tokens < threshold);
        Some(Self {
            prompt_tokens,
            configured_threshold,
            admission_budget,
            force_compaction,
        })
    }

    fn is_over_admission_budget(self, prompt_tokens: usize) -> bool {
        self.admission_budget.is_some_and(|budget| prompt_tokens >= budget)
    }
}

struct CompactionSessionSnapshot {
    thread: ThreadRuntimeHandle,
    session_id: String,
    history: Vec<Message>,
    auto_compact_suppressed: u8,
}

impl CompactionSessionSnapshot {
    fn load(session: &SessionHandle) -> Result<Self> {
        let data = session
            .data
            .lock()
            .map_err(|error| anyhow::anyhow!("ACP session lock poisoned: {error}"))?;
        Ok(Self {
            thread: data.thread.clone(),
            session_id: data.session_id.to_string(),
            history: data.thread.messages(),
            auto_compact_suppressed: data.auto_compact_suppressed,
        })
    }
}

struct CompactionRun<'a> {
    session: &'a SessionHandle,
    provider: &'a dyn LLMProvider,
    runtime: &'a ProviderRequestRuntime,
    model: &'a str,
    vt_config: &'a VTCodeConfig,
    decision: CompactionDecision,
    snapshot: CompactionSessionSnapshot,
}

struct CompactionAttempt {
    outcome: Option<AutoCompactionOutcome>,
    thread: ThreadRuntimeHandle,
    history: Vec<Message>,
}

struct CompactionCommit<'a> {
    session: &'a SessionHandle,
    thread: ThreadRuntimeHandle,
    history: Vec<Message>,
    decision: CompactionDecision,
    original_history_tokens: usize,
}

async fn run_pre_compact_hooks(
    session: &SessionHandle,
    provider: &dyn LLMProvider,
    model: &str,
    original_len: usize,
) -> Result<()> {
    let Some(hooks) = session
        .lifecycle_hooks()
        .context("reading ACP session lifecycle hooks for compaction")?
    else {
        return Ok(());
    };

    let compaction_mode = lifecycle_compaction_mode(manual_compaction_strategy(provider, model));
    let outcome = hooks
        .run_pre_compact(CompactionTrigger::Auto, compaction_mode, original_len, 0, None)
        .await
        .context("ACP PreCompact hooks failed")?;
    for message in outcome.messages {
        warn_hook_message("PreCompact", &message);
    }
    Ok(())
}

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
            () = session.cancellation.cancelled() => Err(anyhow::anyhow!("ACP compaction cancelled")),
            () = sleep_until(deadline) => Err(anyhow::anyhow!("ACP compaction exceeded the provider total-generation timeout")),
            result = compaction => result.context("ACP automatic context compaction failed"),
        }
    } else {
        tokio::select! {
            () = session.cancellation.cancelled() => Err(anyhow::anyhow!("ACP compaction cancelled")),
            result = compaction => result.context("ACP automatic context compaction failed"),
        }
    }
}

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
            anyhow::bail!(
                "ACP prompt uses an estimated {} tokens, exceeds its safe provider admission budget, and automatic compaction made no change",
                decision.prompt_tokens
            );
        }
        return Ok(None);
    };
    Ok(Some(outcome))
}

fn lifecycle_compaction_mode(strategy: CompactionStrategy) -> CompactionMode {
    match strategy {
        CompactionStrategy::Local => CompactionMode::Local,
        CompactionStrategy::NativeStandalone | CompactionStrategy::NativeInline => CompactionMode::Provider,
    }
}

#[cfg(test)]
#[path = "compaction/tests.rs"]
mod tests;

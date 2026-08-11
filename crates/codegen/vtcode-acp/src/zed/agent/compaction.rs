use super::super::types::SessionHandle;
use super::ZedAgent;
use crate::zed::provider_runtime::{ProviderAdmissionError, ProviderRequestRuntime};
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::time::{Instant, sleep_until};
use tracing::{info, warn};
use vtcode_core::compaction::auto::{AutoCompactionInput, auto_compact_messages};
use vtcode_core::compaction::memory_envelope::{
    MemoryEnvelopePlacement, effective_compaction_threshold, local_compaction_config,
};
use vtcode_core::compaction::{CompactionStrategy, ManualCompactionOptions, manual_compaction_strategy};
use vtcode_core::exec::events::{CompactionMode, CompactionTrigger};
use vtcode_core::llm::provider::{LLMProvider, Message, ToolDefinition};

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

    let output_reserve = DEFAULT_OUTPUT_RESERVE_TOKENS.max(context_size / 10);
    Some(
        context_size
            .saturating_sub(output_reserve)
            .saturating_sub(ADMISSION_SAFETY_MARGIN_TOKENS)
            .max(1),
    )
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

impl ZedAgent {
    pub(super) async fn maybe_compact_session(
        &self,
        session: &SessionHandle,
        provider: &dyn LLMProvider,
        runtime: &ProviderRequestRuntime,
        model: &str,
        tools: Option<&Arc<Vec<ToolDefinition>>>,
    ) -> Result<bool> {
        let Some(vt_config) = self.vt_config.as_ref() else {
            return Ok(false);
        };

        let resolved_messages = self.resolved_messages(session);
        let prompt_tokens = estimated_prompt_tokens(&resolved_messages, tools);
        let configured_threshold = effective_compaction_threshold(Some(vt_config), provider, model);
        let admission_budget = admission_prompt_budget(provider, model);
        let trigger = configured_threshold.into_iter().chain(admission_budget).min();
        if trigger.is_none_or(|budget| prompt_tokens < budget) {
            return Ok(false);
        }

        let (thread, session_id, mut history, mut auto_compact_suppressed) = {
            let data = session
                .data
                .lock()
                .map_err(|error| anyhow::anyhow!("ACP session lock poisoned: {error}"))?;
            (
                data.thread.clone(),
                data.session_id.to_string(),
                data.thread.messages(),
                data.auto_compact_suppressed,
            )
        };
        let original_len = history.len();
        let force_compaction = admission_budget.is_some_and(|budget| prompt_tokens >= budget)
            && configured_threshold.is_none_or(|threshold| prompt_tokens < threshold);

        if let Some(hooks) = session.lifecycle_hooks() {
            let compaction_mode = lifecycle_compaction_mode(manual_compaction_strategy(provider, model));
            let outcome = hooks
                .run_pre_compact(CompactionTrigger::Auto, compaction_mode, original_len, 0, None)
                .await
                .context("ACP PreCompact hooks failed")?;
            for message in outcome.messages {
                warn!(level = ?message.level, message = %message.text, "ACP PreCompact hook");
            }
        }

        let permit = runtime.acquire(&session.cancellation).await.map_err(|error| match error {
            ProviderAdmissionError::Cancelled => anyhow::anyhow!("ACP compaction cancelled"),
            other => anyhow::anyhow!(other.to_string()),
        })?;
        let total_deadline = runtime
            .deadline_policy()
            .total_generation
            .map(|timeout| Instant::now() + timeout);
        let outcome_result = {
            let compaction = auto_compact_messages(
                AutoCompactionInput {
                    provider,
                    model,
                    session_id: &session_id,
                    workspace_root: self.config.workspace.as_path(),
                    vt_cfg: Some(vt_config),
                    current_token_usage: prompt_tokens,
                    touched_files: &[],
                    engine_cfg: local_compaction_config(Some(vt_config), false),
                    manual_options: ManualCompactionOptions {
                        max_output_tokens: Some(COMPACTION_SUMMARY_MAX_TOKENS),
                        ..ManualCompactionOptions::default()
                    },
                    placement: MemoryEnvelopePlacement::BeforeLastUserOrSummary,
                    prefire: None,
                    auto_compact_suppressed: &mut auto_compact_suppressed,
                    force_compaction,
                    steering_update: None,
                },
                &mut history,
            );
            tokio::pin!(compaction);
            if let Some(deadline) = total_deadline {
                tokio::select! {
                    () = session.cancellation.cancelled() => Err(anyhow::anyhow!("ACP compaction cancelled")),
                    () = sleep_until(deadline) => Err(anyhow::anyhow!("ACP compaction exceeded the provider total-generation timeout")),
                    result = &mut compaction => result.context("ACP automatic context compaction failed"),
                }
            } else {
                tokio::select! {
                    () = session.cancellation.cancelled() => Err(anyhow::anyhow!("ACP compaction cancelled")),
                    result = &mut compaction => result.context("ACP automatic context compaction failed"),
                }
            }
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

        let Some(outcome) = outcome else {
            warn!(
                prompt_tokens,
                ?configured_threshold,
                ?admission_budget,
                "ACP context pressure crossed its admission budget but compaction made no change"
            );
            if admission_budget.is_some_and(|budget| prompt_tokens >= budget) {
                anyhow::bail!(
                    "ACP prompt uses an estimated {prompt_tokens} tokens, exceeds its safe provider admission budget, and automatic compaction made no change"
                );
            }
            return Ok(false);
        };

        thread.replace_messages(history);
        if let Err(error) = self.checkpoint_session(session).await {
            warn!(%error, "Failed to persist compacted ACP session checkpoint");
        }
        let compacted_prompt_tokens = estimated_prompt_tokens(&self.resolved_messages(session), tools);
        if admission_budget.is_some_and(|budget| compacted_prompt_tokens >= budget) {
            anyhow::bail!(
                "ACP compaction reduced the prompt from an estimated {prompt_tokens} to {compacted_prompt_tokens} tokens, which still exceeds its safe provider admission budget"
            );
        }
        info!(
            provider = provider.name(),
            model,
            prompt_tokens,
            compacted_prompt_tokens,
            original_len,
            compacted_len = outcome.compacted_len,
            compaction_mode = outcome.mode.as_str(),
            ?configured_threshold,
            ?admission_budget,
            "Applied automatic ACP conversation compaction"
        );
        Ok(true)
    }
}

fn lifecycle_compaction_mode(strategy: CompactionStrategy) -> CompactionMode {
    match strategy {
        CompactionStrategy::Local => CompactionMode::Local,
        CompactionStrategy::NativeStandalone | CompactionStrategy::NativeInline => CompactionMode::Provider,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use vtcode_core::llm::provider::{LLMError, LLMRequest, LLMResponse};

    struct ContextProvider {
        context_size: usize,
    }

    #[async_trait]
    impl LLMProvider for ContextProvider {
        fn name(&self) -> &str {
            "context-test"
        }

        fn effective_context_size(&self, _model: &str) -> usize {
            self.context_size
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            panic!("pure admission tests do not generate")
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["test".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }
    }

    #[test]
    fn arli_boundary_compacts_before_output_reservation_overflows_context() {
        let provider = ContextProvider { context_size: 524_288 };

        assert_eq!(admission_prompt_budget(&provider, "test"), Some(470_836));
        assert!(should_compact(&provider, "test", 507_905));
    }

    #[test]
    fn prompt_below_soft_context_budget_does_not_compact() {
        let provider = ContextProvider { context_size: 524_288 };

        assert!(!should_compact(&provider, "test", 400_000));
    }

    #[test]
    fn small_context_reserves_at_least_default_output_and_margin() {
        let provider = ContextProvider { context_size: 32_768 };

        assert_eq!(admission_prompt_budget(&provider, "test"), Some(15_360));
    }

    #[test]
    fn lifecycle_compaction_mode_preserves_local_and_native_strategy() {
        assert_eq!(lifecycle_compaction_mode(CompactionStrategy::Local), CompactionMode::Local);
        assert_eq!(lifecycle_compaction_mode(CompactionStrategy::NativeStandalone), CompactionMode::Provider);
        assert_eq!(lifecycle_compaction_mode(CompactionStrategy::NativeInline), CompactionMode::Provider);
    }
}

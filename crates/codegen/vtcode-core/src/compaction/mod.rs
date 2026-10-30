//! Compact provider/local history without losing the original task or latest steering.
//!
//! This facade owns provider strategy and summary requests. `context_budget`
//! admits model-window budgets, `history` composes retained turns and summaries,
//! `bounded_history` limits optional payloads, and `protocol` selects valid tails.
//! Callers publish history only after compaction and their own admission succeed.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use vtcode_commons::llm::FinishReason;
use vtcode_config::constants::context::DEFAULT_COMPACTION_TRIGGER_RATIO;

use crate::config::types::{ReasoningEffortLevel, VerbosityLevel};
use crate::exec::events::CompactionMode;
#[cfg(test)]
use crate::llm::provider::MessageRole;
use crate::llm::provider::{LLMProvider, LLMRequest, Message, MessageContent, ResponsesCompactionOptions};

pub mod auto;
mod bounded_history;
mod context_budget;
mod history;
pub mod memory_envelope;
pub mod prefire;
mod protocol;
mod telemetry;
pub mod two_pass;

#[cfg(test)]
use bounded_history::{bounded_protocol_group, select_bounded_history};
pub(super) use context_budget::is_protected_user_budget_error;
use context_budget::{
    bound_compacted_history_to_context, compaction_history_budget, context_bounded_compaction_config,
    ensure_protected_user_budget, summary_history_for_request,
};
#[cfg(test)]
use history::retention::{UserRetentionPolicy, coherence_tool_call_pairs, collect_retained_user_messages};
pub(crate) use history::{build_local_compacted_history, build_summary_prompt};
use history::{build_summary_compacted_history, normalize_provider_compacted_history};
#[cfg(test)]
use protocol::{complete_protocol_prefix, continuity_tail};

pub use crate::compaction::memory_envelope::{effective_context_budget, effective_session_context_budget};
pub use crate::compaction::prefire::{AsyncCompactionCache, PrefireState};

pub const SUPPRESS_NONE: u8 = 0;
pub const SUPPRESS_TURN: u8 = 1;
pub const SUPPRESS_STICKY: u8 = 2;
pub const SUPPRESS_UNTIL_SUCCESS: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SuppressReason {
    CreditBlock,
    Size,
    Auth,
    Schema,
    Other,
}

impl SuppressReason {
    fn suppress_state(self) -> u8 {
        match self {
            SuppressReason::Size | SuppressReason::Schema => SUPPRESS_STICKY,
            SuppressReason::CreditBlock | SuppressReason::Auth => SUPPRESS_UNTIL_SUCCESS,
            SuppressReason::Other => SUPPRESS_TURN,
        }
    }
}

/// Classify a deterministic compaction failure's error text into a fixed
/// [`SuppressReason`] (drives telemetry + sticky-vs-per-turn scope).
pub(crate) fn classify_suppress_reason(error_msg: &str) -> SuppressReason {
    let m = error_msg.to_ascii_lowercase();
    if m.contains("spending-limit")
        || m.contains("spending limit")
        || m.contains("out of credits")
        || m.contains("usage balance exhausted")
        || m.contains("usage limit reached")
    {
        SuppressReason::CreditBlock
    } else if m.contains("context length") || m.contains("too many tokens") {
        SuppressReason::Size
    } else if m.contains("status 401") || m.contains("unauthorized") {
        SuppressReason::Auth
    } else if m.contains("invalid_request_error") {
        SuppressReason::Schema
    } else {
        SuppressReason::Other
    }
}

const DEFAULT_COMPACTION_TARGET_THRESHOLD: f64 = 0.50;
const DEFAULT_COMPACTION_KEEP_LAST_MESSAGES: usize = 10;
const DEFAULT_RETAINED_USER_MESSAGE_TOKENS: usize = 20_000;
const DEFAULT_RETAINED_USER_MESSAGES: usize = 6;
/// Internal continuity budget. This is deliberately not configuration: changing
/// it changes the shape of every compacted request and therefore provider cache
/// behavior.
const CONTINUITY_TAIL_TARGET_TOKENS: usize = 20_000;
const SUMMARY_PREFIX: &str = "Previous conversation summary:\n";
const ABSTRACT_PREFIX: &str = "Earlier context (abstract):\n";
const DETAIL_PREFIX: &str = "Recent context (summary):\n";

/// Default summarization prompt. Structures the summary for continuity: after
/// reading it, the next context must feel like a seamless continuation, not a
/// fresh start. Kept as a `const` so `CompactionConfig::default` does not
/// re-allocate a ~1KB literal on every construction.
const DEFAULT_SUMMARY_PROMPT: &str = "Summarize the conversation so far using this exact structure. The goal is continuity: after reading this summary, the next context must feel like a seamless continuation of the same task, not a fresh start.\n\n## Goal\n[What the user is trying to accomplish]\n\n## Constraints & Preferences\n- [Requirements, preferences, or constraints from the user]\n\n## What I Was Just Doing\n[The single most recent action in progress: what step the agent was executing, which tool or edit was underway, and where it left off. This is the continuity anchor.]\n\n## Last Action & Result\n[The last completed action and its outcome (success, error, or partial). Include the exact error or status if relevant.]\n\n## Progress\n### Done\n- [Completed work]\n\n### In Progress\n- [Current work]\n\n### Blocked\n- [Blocking issues, if any]\n\n## Key Decisions\n- **[Decision]**: [Reason]\n\n## Next Steps\n1. [Most important next step]\n\n## Critical Context\n- [Facts needed to continue]\n\nKeep it concise and actionable. Always preserve the current task objective and acceptance criteria, file paths that were read or modified, test results and error messages, and decisions with their reasoning.";

/// Compaction configuration for context window management.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Threshold (0.0-1.0) at which to trigger compaction.
    pub trigger_threshold: f64,
    /// Target usage ratio (0.0-1.0) after compaction.
    pub target_threshold: f64,
    /// Prompt for summarization.
    pub summary_prompt: String,
    /// Legacy short-circuit used to skip local compaction for tiny histories.
    pub keep_last_messages: usize,
    /// Soft token budget for optional historical messages; the original task
    /// and latest user instructions remain complete up to the model budget.
    pub retained_user_message_tokens: usize,
    /// Soft cap on optional retained messages; protected user instructions
    /// and complete protocol groups take precedence.
    pub retained_user_messages: usize,
    /// Force local summarization even for short histories and providers with native compaction.
    pub always_summarize: bool,
    /// Enable hierarchical summarization (multi-level pyramid).
    ///
    /// When `true`, compaction produces three tiers instead of a flat summary:
    /// - **Abstract**: oldest turns compressed into 1-2 sentences
    /// - **Detail**: middle turns summarized into a paragraph
    /// - **Verbatim**: most recent turns kept as-is
    ///
    /// When `false` (default), all old turns become a single flat summary.
    pub hierarchical: bool,
    /// Auto-compaction suppression state. `SUPPRESS_NONE` (0) means compaction
    /// is allowed; any other value gates automatic compaction until the
    /// appropriate clearing event (success, model switch, etc.).
    pub auto_compact_suppressed: u8,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            trigger_threshold: DEFAULT_COMPACTION_TRIGGER_RATIO,
            target_threshold: DEFAULT_COMPACTION_TARGET_THRESHOLD,
            summary_prompt: DEFAULT_SUMMARY_PROMPT.to_string(),
            keep_last_messages: DEFAULT_COMPACTION_KEEP_LAST_MESSAGES,
            retained_user_message_tokens: DEFAULT_RETAINED_USER_MESSAGE_TOKENS,
            retained_user_messages: DEFAULT_RETAINED_USER_MESSAGES,
            always_summarize: false,
            hierarchical: false,
            auto_compact_suppressed: SUPPRESS_NONE,
        }
    }
}

/// Compact conversation history using the configured summarizer.
#[cfg_attr(feature = "profiling", hotpath::measure)]
pub async fn compact_history(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
) -> Result<Vec<Message>> {
    if history.is_empty() {
        return Ok(Vec::new());
    }

    ensure_protected_user_budget(history, provider, model)?;

    if !config.always_summarize && history.len() <= config.keep_last_messages {
        return Ok(history.to_vec());
    }

    if !config.always_summarize && provider.supports_responses_compaction(model) {
        let compacted = provider
            .compact_history(model, history)
            .await
            .context("Failed to compact history via Responses compact endpoint")?;
        return bound_compacted_history_to_context(
            normalize_provider_compacted_history(compacted, history),
            provider,
            model,
        );
    }

    let effective_config = context_bounded_compaction_config(provider, model, history, config);
    let summary_history = summary_history_for_request(history, provider, model)?;
    let summary_prompt = build_summary_prompt(summary_history.as_ref(), &effective_config.summary_prompt);
    let request = LLMRequest {
        messages: std::sync::Arc::new(vec![Message::user(summary_prompt)]),
        model: model.to_string(),
        ..Default::default()
    };

    let response = provider
        .generate(request)
        .await
        .context("Failed to generate compaction summary")?;

    let summary = response.content.unwrap_or_default().trim().to_string();
    bound_compacted_history_to_context(
        build_local_compacted_history(
            history,
            &summary,
            &effective_config,
            // Keep the same protocol-safe continuity tail as the live/manual
            // paths. Forked histories must not lose the newest working turn.
            true,
        ),
        provider,
        model,
    )
}

/// How the manual `/compact` command compacts for a given provider/model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionStrategy {
    /// Provider exposes a standalone on-demand compaction endpoint
    /// (OpenAI `/responses/compact`). Delegates to `LLMProvider::compact_history_with_options`.
    NativeStandalone,
    /// Provider compacts inline via request fields, threshold-triggered
    /// (Anthropic `compact_20260112`). Invoked through `LLMProvider::generate`
    /// with `context_management` set and `pause_after_compaction`.
    NativeInline,
    /// Universal fallback: summarize history via `LLMProvider::generate` and
    /// rebuild as a summary message plus retained recent user messages.
    /// Works for every provider.
    Local,
}

/// Select the manual-compaction strategy for a provider/model.
///
/// `NativeStandalone` when the provider opts in via `supports_manual_openai_compaction`
/// (e.g. OpenAI `/responses/compact`), `NativeInline` when the provider reports
/// inline compaction support via `supports_native_inline_compaction` (e.g.
/// Anthropic `compact_20260112`), otherwise `Local`.
///
/// Note: `supports_responses_compaction` is intentionally *not* the discriminator
/// for `NativeInline`. It is overloaded — true for both OpenAI-compatible
/// standalone compaction and Anthropic inline compaction — so OpenAI-compatible
/// custom endpoints (which report it but cannot serve an Anthropic
/// `compact_20260112` edit) would otherwise be misrouted to `NativeInline` and
/// waste a rejected `generate` call before falling back to `Local`.
pub fn manual_compaction_strategy(provider: &dyn LLMProvider, model: &str) -> CompactionStrategy {
    if provider.supports_manual_openai_compaction(model) {
        CompactionStrategy::NativeStandalone
    } else if provider.supports_native_inline_compaction(model) {
        CompactionStrategy::NativeInline
    } else {
        CompactionStrategy::Local
    }
}

/// Universally meaningful manual-compaction options.
///
/// Provider-specific extras (OpenAI `service_tier` / `prompt_cache_key` / `store` /
/// `include`) are intentionally absent: the manual `/compact` command exposes only
/// the options that apply across every provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManualCompactionOptions {
    /// Overrides the default summary/compaction prompt when set.
    pub instructions: Option<String>,
    /// Caps the summary/compaction output length on every provider.
    pub max_output_tokens: Option<u32>,
    /// Optional reasoning effort override for the compaction pass.
    pub reasoning_effort: Option<ReasoningEffortLevel>,
    /// Optional verbosity override for the compaction output.
    pub verbosity: Option<VerbosityLevel>,
}

impl From<ManualCompactionOptions> for ResponsesCompactionOptions {
    fn from(options: ManualCompactionOptions) -> Self {
        Self {
            instructions: options.instructions,
            max_output_tokens: options.max_output_tokens,
            reasoning_effort: options.reasoning_effort,
            verbosity: options.verbosity,
            responses_include: None,
            response_store: None,
            service_tier: None,
            prompt_cache_key: None,
        }
    }
}

impl CompactionConfig {
    /// Return a config with the manual options' instructions applied as the
    /// summary prompt override. The remaining option fields
    /// (`max_output_tokens`, `reasoning_effort`, `verbosity`) are applied to the
    /// summary `LLMRequest` directly by `summarize_locally`, not stored here.
    fn with_manual_overrides(self, options: &ManualCompactionOptions) -> Self {
        let summary_prompt = options
            .instructions
            .clone()
            .map(|instructions| instructions.trim().to_string())
            .filter(|instructions| !instructions.is_empty())
            .unwrap_or(self.summary_prompt);
        Self { summary_prompt, ..self }
    }
}

/// Compact history for the manual `/compact` command using provider-native
/// compaction when available, falling back to local summarization otherwise.
///
/// Returns the compacted messages and the `CompactionMode` that produced them
/// (`Provider` for native compaction, `Local` for client-side summarization).
#[cfg_attr(feature = "profiling", hotpath::measure)]
pub async fn compact_history_manual(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
    options: &ManualCompactionOptions,
) -> Result<(Vec<Message>, CompactionMode)> {
    if history.is_empty() {
        return Ok((Vec::new(), CompactionMode::Local));
    }
    ensure_protected_user_budget(history, provider, model)?;
    match manual_compaction_strategy(provider, model) {
        CompactionStrategy::NativeStandalone => {
            let responses_options: ResponsesCompactionOptions = options.clone().into();
            let compacted = provider
                .compact_history_with_options(model, history, &responses_options)
                .await
                .context("Failed to compact history via provider-native compaction")?;
            Ok((
                bound_compacted_history_to_context(
                    normalize_provider_compacted_history(compacted, history),
                    provider,
                    model,
                )?,
                CompactionMode::Provider,
            ))
        }
        CompactionStrategy::NativeInline => {
            compact_history_native_inline(provider, model, history, config, options).await
        }
        CompactionStrategy::Local => {
            let compacted = summarize_locally(provider, model, history, config, options).await?;
            Ok((compacted, CompactionMode::Local))
        }
    }
}

/// Native inline compaction (Anthropic `compact_20260112`).
///
/// Forces a compaction pass by setting the minimum trigger threshold with
/// `pause_after_compaction: true`, so the response contains only the compaction
/// block. If compaction does not fire (history below the provider's minimum
/// trigger, currently 50k tokens for Anthropic), transparently falls back to
/// local summarization so the manual command always succeeds.
async fn compact_history_native_inline(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
    options: &ManualCompactionOptions,
) -> Result<(Vec<Message>, CompactionMode)> {
    const ANTHROPIC_COMPACT_TRIGGER_FLOOR: u64 = 50_000;

    let mut compact_edit = serde_json::Map::new();
    compact_edit.insert("type".to_string(), json!("compact_20260112"));
    compact_edit
        .insert("trigger".to_string(), json!({ "type": "input_tokens", "value": ANTHROPIC_COMPACT_TRIGGER_FLOOR }));
    compact_edit.insert("pause_after_compaction".to_string(), json!(true));
    if let Some(instructions) = options
        .instructions
        .as_ref()
        .map(|instructions| instructions.trim())
        .filter(|instructions| !instructions.is_empty())
    {
        compact_edit.insert("instructions".to_string(), json!(instructions));
    }

    let request = LLMRequest {
        messages: std::sync::Arc::new(history.to_vec()),
        model: model.to_string(),
        context_management: Some(json!({ "edits": [Value::Object(compact_edit)] })),
        max_tokens: options.max_output_tokens,
        reasoning_effort: options.reasoning_effort,
        verbosity: options.verbosity,
        ..Default::default()
    };

    // The inline compaction request is Anthropic-specific (`compact_20260112`).
    // If the provider does not actually support inline compaction (e.g. it was
    // selected because it exposes a different standalone Responses compact
    // endpoint) the request may be rejected. Per the manual `/compact` contract
    // ("always succeeds"), swallow the inline error and fall back to local
    // summarization rather than aborting the whole command.
    let response = match provider.generate(request).await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(
                error = %error,
                "provider-native inline compaction request failed; \
                 falling back to local summarization"
            );
            let compacted = summarize_locally(provider, model, history, config, options).await?;
            return Ok((compacted, CompactionMode::Local));
        }
    };

    if response.finish_reason == FinishReason::Pause
        && let Some(summary) = response
            .compaction
            .as_ref()
            .map(|summary| summary.trim())
            .filter(|summary| !summary.is_empty())
    {
        let effective_config = context_bounded_compaction_config(provider, model, history, config);
        let compacted = build_summary_compacted_history(history, summary, &effective_config, true);
        return Ok((bound_compacted_history_to_context(compacted, provider, model)?, CompactionMode::Provider));
    }

    // Compaction did not fire (e.g. history below the minimum trigger threshold);
    // fall back to local summarization so the manual command always succeeds.
    let compacted = summarize_locally(provider, model, history, config, options).await?;
    Ok((compacted, CompactionMode::Local))
}

/// Local (provider-agnostic) summarization compaction.
///
/// Builds a summary prompt from the history, asks the provider to summarize via
/// `generate`, and rebuilds the history as a summary system message plus the
/// retained recent user messages. Applies the manual options to the summary
/// request.
///
/// When `config.hierarchical` is `true`, delegates to
/// [`summarize_locally_hierarchical`] which produces a multi-tier pyramid
/// (abstract + detail + verbatim) instead of a flat summary.
async fn summarize_locally(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
    options: &ManualCompactionOptions,
) -> Result<Vec<Message>> {
    if config.hierarchical {
        return summarize_locally_hierarchical(provider, model, history, config, options).await;
    }

    let effective_config =
        context_bounded_compaction_config(provider, model, history, &config.clone().with_manual_overrides(options));
    let summary_history = summary_history_for_request(history, provider, model)?;
    let summary_prompt = build_summary_prompt(summary_history.as_ref(), &effective_config.summary_prompt);
    let request = LLMRequest {
        messages: std::sync::Arc::new(vec![Message::user(summary_prompt)]),
        model: model.to_string(),
        max_tokens: options.max_output_tokens,
        reasoning_effort: options.reasoning_effort,
        verbosity: options.verbosity,
        ..Default::default()
    };

    let response = provider
        .generate(request)
        .await
        .context("Failed to generate compaction summary")?;

    let summary = response.content.unwrap_or_default().trim().to_string();
    bound_compacted_history_to_context(
        build_summary_compacted_history(history, summary, &effective_config, true),
        provider,
        model,
    )
}

/// Hierarchical local summarization: abstract + detail + verbatim pyramid.
///
/// Splits the history into three bands and summarizes each with a different
/// compression target:
/// - **Abstract** (oldest third): 1-2 sentence overview
/// - **Detail** (remaining history): paragraph-level summary, including recent work
/// - **Verbatim**: protected user instructions plus bounded recent protocol work
///
/// This follows the hierarchical summarization strategy from the context window
/// management literature: recent turns verbatim, older turns as paragraph
/// summaries, oldest turns as a single abstract.
async fn summarize_locally_hierarchical(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
    options: &ManualCompactionOptions,
) -> Result<Vec<Message>> {
    let effective_config =
        context_bounded_compaction_config(provider, model, history, &config.clone().with_manual_overrides(options));
    let summary_history = summary_history_for_request(history, provider, model)?;

    // Abstract the oldest third; detail also covers the active turn.
    let total = summary_history.len();
    let band_size = total / 3;
    let abstract_end = band_size;

    // Band 1 (oldest): compress into 1-2 sentence abstract.
    let abstract_band = &summary_history[..abstract_end];
    let abstract_prompt = format!(
        "In 1-2 sentences, what was the overall goal and major progress in this \
         portion of the conversation?\n\n{}",
        build_summary_prompt(abstract_band, ""),
    );
    let abstract_request = LLMRequest {
        messages: std::sync::Arc::new(vec![Message::user(abstract_prompt)]),
        model: model.to_string(),
        max_tokens: Some(150),
        reasoning_effort: options.reasoning_effort,
        verbosity: options.verbosity,
        ..Default::default()
    };
    let abstract_response = provider
        .generate(abstract_request)
        .await
        .context("Failed to generate abstract summary")?;
    let abstract_summary = abstract_response.content.unwrap_or_default().trim().to_string();

    // Include recent work in the detail summary even when its protocol tail
    // needs a bounded preview.
    let detail_band = &summary_history[abstract_end..];
    let detail_prompt = build_summary_prompt(detail_band, &effective_config.summary_prompt);
    let detail_request = LLMRequest {
        messages: std::sync::Arc::new(vec![Message::user(detail_prompt)]),
        model: model.to_string(),
        max_tokens: options.max_output_tokens,
        reasoning_effort: options.reasoning_effort,
        verbosity: options.verbosity,
        ..Default::default()
    };
    let detail_response = provider
        .generate(detail_request)
        .await
        .context("Failed to generate detail summary")?;
    let detail_summary = detail_response.content.unwrap_or_default().trim().to_string();

    // Both summaries cover the active turn too; retain the authoritative user
    // instructions and bounded protocol work through the same builder as the
    // single-pass path.
    let mut new_history = build_summary_compacted_history(history, &detail_summary, &effective_config, true);
    new_history.insert(0, Message::system(format!("{ABSTRACT_PREFIX}{abstract_summary}")));
    if let Some(detail) = new_history.get_mut(1) {
        detail.content = MessageContent::Text(format!("{DETAIL_PREFIX}{detail_summary}"));
    }
    bound_compacted_history_to_context(new_history, provider, model)
}

#[cfg(test)]
mod tests {
    use super::{
        CompactionConfig, ManualCompactionOptions, compact_history, compact_history_manual, continuity_tail,
        manual_compaction_strategy,
    };
    use crate::config::types::{ReasoningEffortLevel, VerbosityLevel};
    use crate::exec::events::CompactionMode;
    use crate::llm::provider::{
        LLMError, LLMProvider, LLMRequest, LLMResponse, Message, MessageRole, ResponsesCompactionOptions,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;
    use vtcode_commons::llm::{FinishReason, ToolCall};

    struct StubProvider;

    struct NativeCompactionProvider;

    /// Provider that opts into the standalone manual-compaction path
    /// (`supports_manual_openai_compaction -> true`), e.g. OpenAI `/responses/compact`.
    struct ManualStandaloneProvider {
        last_options: Mutex<Option<ResponsesCompactionOptions>>,
    }

    /// Inline-compaction-capable provider (`supports_responses_compaction -> true`,
    /// `supports_manual_openai_compaction -> false`), e.g. Anthropic `compact_20260112`.
    /// Returns a `Pause` finish with a compaction block so the inline path succeeds.
    struct InlinePauseProvider {
        last_request: Mutex<Option<LLMRequest>>,
    }

    /// Capturing provider with no native support; used to assert the Local summary
    /// request carries the manual options.
    struct CapturingProvider {
        last_request: Mutex<Option<LLMRequest>>,
    }

    /// Inline-dispatched provider whose inline `generate` rejects the Anthropic
    /// `compact_20260112` edit. Models providers that report
    /// `supports_responses_compaction` but are not Anthropic-style inline
    /// compactors; the dispatch must fall back to Local rather than aborting.
    struct InlineRejectingProvider;

    /// Models an OpenAI-compatible custom endpoint (non-`api.openai.com` host or
    /// `provider_key_override`): it exposes the Responses API
    /// (`supports_responses_compaction == true`) but neither the standalone
    /// `/responses/compact` endpoint nor Anthropic inline compaction. The dispatch
    /// must pick `Local` rather than misrouting it to `NativeInline` (which would
    /// send an Anthropic `compact_20260112` edit only to be rejected).
    struct CompatibleEndpointProvider;

    #[async_trait]
    impl LLMProvider for StubProvider {
        fn name(&self) -> &str {
            "stub"
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        fn effective_context_size(&self, _model: &str) -> usize {
            32_768
        }
    }

    #[async_trait]
    impl LLMProvider for NativeCompactionProvider {
        fn name(&self) -> &str {
            "native"
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        fn supports_responses_compaction(&self, _model: &str) -> bool {
            true
        }

        async fn compact_history(&self, _model: &str, _history: &[Message]) -> Result<Vec<Message>, LLMError> {
            Ok(vec![Message::system("provider compacted".to_string())])
        }
    }

    #[async_trait]
    impl LLMProvider for ManualStandaloneProvider {
        fn name(&self) -> &str {
            "manual-standalone"
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        fn supports_manual_openai_compaction(&self, _model: &str) -> bool {
            true
        }

        async fn compact_history_with_options(
            &self,
            _model: &str,
            _history: &[Message],
            options: &ResponsesCompactionOptions,
        ) -> Result<Vec<Message>, LLMError> {
            *self.last_options.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(options.clone());
            Ok(vec![Message::system("provider standalone compacted".to_string())])
        }
    }

    #[async_trait]
    impl LLMProvider for InlinePauseProvider {
        fn name(&self) -> &str {
            "inline-pause"
        }

        async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
            *self.last_request.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request);
            let mut response = LLMResponse::new("stub-model", "compacted by provider");
            response.finish_reason = FinishReason::Pause;
            response.compaction = Some("provider compaction summary".to_string());
            Ok(response)
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        fn supports_responses_compaction(&self, _model: &str) -> bool {
            true
        }

        fn supports_native_inline_compaction(&self, _model: &str) -> bool {
            true
        }
    }

    #[async_trait]
    impl LLMProvider for CapturingProvider {
        fn name(&self) -> &str {
            "capturing"
        }

        async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
            *self.last_request.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request);
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }
    }

    #[async_trait]
    impl LLMProvider for InlineRejectingProvider {
        fn name(&self) -> &str {
            "inline-rejecting"
        }

        async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
            // Reject only the inline compaction request (carries the Anthropic
            // `compact_20260112` edit); the Local summary request must succeed.
            if request.context_management.is_some() {
                return Err(LLMError::Provider {
                    message: "provider rejected inline compact edit".to_string(),
                    metadata: None,
                });
            }
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        fn supports_responses_compaction(&self, _model: &str) -> bool {
            true
        }

        fn supports_native_inline_compaction(&self, _model: &str) -> bool {
            true
        }
    }

    #[async_trait]
    impl LLMProvider for CompatibleEndpointProvider {
        fn name(&self) -> &str {
            "compatible-endpoint"
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            Ok(LLMResponse::new("stub-model", "summary"))
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["stub-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        // Reports Responses API support but neither standalone nor inline
        // compaction (defaults: supports_manual_openai_compaction and
        // supports_native_inline_compaction are both false).
        fn supports_responses_compaction(&self, _model: &str) -> bool {
            true
        }
    }

    fn sample_history() -> Vec<Message> {
        vec![
            Message::assistant("setup".to_string()),
            Message::user("first request".to_string()),
            Message::assistant("working".to_string()),
            Message::user("second request".to_string()),
        ]
    }

    /// Build an assistant message that carries a (single) pending tool call.
    fn assistant_with_calls(content: &str, call_id: &str) -> Message {
        let mut message = Message::assistant(content.to_string());
        message.tool_calls = Some(vec![ToolCall {
            id: call_id.to_string(),
            call_type: "function".to_string(),
            function: None,
            text: None,
            thought_signature: None,
        }]);
        message
    }

    #[test]
    fn collect_retained_keeps_tool_result_with_its_assistant() {
        // When the assistant tool-call turn is retained, its tool result must
        // survive so the turn stays coherent (the model sees each call's return).
        let history = vec![
            Message::user("u1".to_string()),
            assistant_with_calls("calling tool", "c1"),
            Message::tool_response("c1".to_string(), "r1".to_string()),
            Message::user("u2".to_string()),
        ];
        let retained =
            super::collect_retained_user_messages(&history, 20_000, 4, super::UserRetentionPolicy::WholeHistory);
        assert!(retained.iter().any(|m| m.content.as_text().contains("u1")));
        assert!(retained.iter().any(|m| m.content.as_text().contains("u2")));
        assert!(
            retained.iter().any(|m| m.content.as_text().contains("r1")),
            "tool result paired with its retained assistant must survive"
        );
    }

    #[test]
    fn collect_retained_drops_orphaned_tool_result() {
        // If the assistant tool-call turn is dropped (over the retention cap),
        // its tool result is orphaned — the model never saw the call — and must
        // not survive compaction, because an orphaned result is invalid to send
        // to a provider.
        let history = vec![
            Message::user("u1".to_string()),
            assistant_with_calls("calling tool", "c1"),
            Message::tool_response("c1".to_string(), "r1".to_string()),
            Message::user("u2".to_string()),
        ];
        let retained =
            super::collect_retained_user_messages(&history, 20_000, 3, super::UserRetentionPolicy::WholeHistory);
        assert!(retained.iter().any(|m| m.content.as_text().contains("u1")));
        assert!(retained.iter().any(|m| m.content.as_text().contains("u2")));
        assert!(!retained.iter().any(|m| m.content.as_text().contains("r1")), "orphaned tool result must be dropped");
    }

    #[tokio::test]
    async fn manual_compaction_strategy_picks_local_for_plain_provider() {
        assert_eq!(manual_compaction_strategy(&StubProvider, "stub-model"), super::CompactionStrategy::Local);
    }

    #[tokio::test]
    async fn manual_compaction_strategy_picks_native_standalone_for_manual_provider() {
        let provider = ManualStandaloneProvider { last_options: Mutex::new(None) };
        assert_eq!(manual_compaction_strategy(&provider, "stub-model"), super::CompactionStrategy::NativeStandalone);
    }

    #[tokio::test]
    async fn manual_compaction_strategy_picks_native_inline_for_responses_capable_provider() {
        let provider = InlinePauseProvider { last_request: Mutex::new(None) };
        assert_eq!(manual_compaction_strategy(&provider, "stub-model"), super::CompactionStrategy::NativeInline);
    }

    #[tokio::test]
    async fn manual_compaction_strategy_picks_local_for_compatible_endpoint() {
        // OpenAI-compatible custom endpoints report `supports_responses_compaction`
        // but cannot serve standalone `/responses/compact` or Anthropic inline
        // compaction; they must route to Local, not NativeInline.
        assert_eq!(
            manual_compaction_strategy(&CompatibleEndpointProvider, "stub-model"),
            super::CompactionStrategy::Local
        );
    }

    #[tokio::test]
    async fn compact_history_manual_uses_local_summary_for_plain_provider() {
        let history = sample_history();
        let config = CompactionConfig {
            always_summarize: true,
            ..CompactionConfig::default()
        };

        let (compacted, mode) =
            compact_history_manual(&StubProvider, "stub-model", &history, &config, &ManualCompactionOptions::default())
                .await
                .expect("manual compaction");

        assert_eq!(mode, CompactionMode::Local);
        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nsummary");
        assert_eq!(compacted[1].content.as_text(), "first request");
        assert_eq!(compacted[2].content.as_text(), "working");
        assert_eq!(compacted[3].content.as_text(), "second request");
    }

    #[tokio::test]
    async fn compact_history_manual_uses_native_standalone_for_manual_provider() {
        let history = sample_history();
        let config = CompactionConfig::default();
        let provider = ManualStandaloneProvider { last_options: Mutex::new(None) };

        let (compacted, mode) =
            compact_history_manual(&provider, "stub-model", &history, &config, &ManualCompactionOptions::default())
                .await
                .expect("manual compaction");

        assert_eq!(mode, CompactionMode::Provider);
        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[0].content.as_text(), "provider standalone compacted");
        assert_eq!(compacted[1].content.as_text(), "first request");
        assert_eq!(compacted[2].content.as_text(), "working");
        assert_eq!(compacted[3].content.as_text(), "second request");
    }

    #[tokio::test]
    async fn compact_history_manual_passes_options_to_native_standalone() {
        let history = sample_history();
        let config = CompactionConfig::default();
        let provider = ManualStandaloneProvider { last_options: Mutex::new(None) };
        let options = ManualCompactionOptions {
            instructions: Some("keep only decisions".to_string()),
            max_output_tokens: Some(256),
            reasoning_effort: Some(ReasoningEffortLevel::Minimal),
            verbosity: Some(VerbosityLevel::High),
        };

        let (_compacted, mode) = compact_history_manual(&provider, "stub-model", &history, &config, &options)
            .await
            .expect("manual compaction");

        assert_eq!(mode, CompactionMode::Provider);
        let captured = provider.last_options.lock().unwrap().clone().expect("captured options");
        assert_eq!(captured.instructions.as_deref(), Some("keep only decisions"));
        assert_eq!(captured.max_output_tokens, Some(256));
        assert_eq!(captured.reasoning_effort, Some(ReasoningEffortLevel::Minimal));
        assert_eq!(captured.verbosity, Some(VerbosityLevel::High));
    }

    #[tokio::test]
    async fn compact_history_manual_uses_native_inline_when_pause_and_compaction_present() {
        let history = sample_history();
        let config = CompactionConfig::default();
        let provider = InlinePauseProvider { last_request: Mutex::new(None) };

        let (compacted, mode) =
            compact_history_manual(&provider, "stub-model", &history, &config, &ManualCompactionOptions::default())
                .await
                .expect("manual compaction");

        assert_eq!(mode, CompactionMode::Provider);
        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nprovider compaction summary");
        assert_eq!(compacted[1].content.as_text(), "first request");
        assert_eq!(compacted[2].content.as_text(), "working");
        assert_eq!(compacted[3].content.as_text(), "second request");

        // The inline request must carry the `compact_20260112` edit with a forced
        // pause so the provider actually performs compaction on demand.
        let captured = provider.last_request.lock().unwrap().clone().expect("captured inline request");
        let context_management = captured
            .context_management
            .as_ref()
            .expect("context_management set on inline compaction request");
        let edit = &context_management["edits"][0];
        assert_eq!(edit["type"].as_str(), Some("compact_20260112"));
        assert_eq!(edit["pause_after_compaction"].as_bool(), Some(true));
        assert_eq!(edit["trigger"]["value"].as_u64(), Some(50_000));
    }

    #[tokio::test]
    async fn compact_history_manual_inline_request_carries_instructions_when_provided() {
        let history = sample_history();
        let config = CompactionConfig::default();
        let provider = InlinePauseProvider { last_request: Mutex::new(None) };
        let options = ManualCompactionOptions {
            instructions: Some("  keep only decisions  ".to_string()),
            ..ManualCompactionOptions::default()
        };

        let (_compacted, _mode) = compact_history_manual(&provider, "stub-model", &history, &config, &options)
            .await
            .expect("manual compaction");

        let captured = provider.last_request.lock().unwrap().clone().expect("captured inline request");
        let edit = &captured.context_management.as_ref().expect("context_management")["edits"][0];
        assert_eq!(edit["instructions"].as_str(), Some("keep only decisions"));
    }

    #[tokio::test]
    async fn compact_history_manual_falls_back_to_local_when_inline_compaction_not_fired() {
        let history = sample_history();
        let config = CompactionConfig::default();

        // NativeCompactionProvider is inline-capable but its `generate` returns a
        // normal `Stop` with no compaction block, so the inline attempt cannot
        // fire and the dispatch must transparently fall back to Local.
        let (compacted, mode) = compact_history_manual(
            &NativeCompactionProvider,
            "stub-model",
            &history,
            &config,
            &ManualCompactionOptions::default(),
        )
        .await
        .expect("manual compaction");

        assert_eq!(mode, CompactionMode::Local);
        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nsummary");
    }

    #[tokio::test]
    async fn compact_history_manual_falls_back_to_local_when_inline_request_errors() {
        let history = sample_history();
        let config = CompactionConfig::default();

        // A provider dispatched to NativeInline that rejects the Anthropic
        // `compact_20260112` edit must not abort the whole command; the dispatch
        // falls back to Local summarization (the manual `/compact` contract:
        // always succeeds).
        let (compacted, mode) = compact_history_manual(
            &InlineRejectingProvider,
            "stub-model",
            &history,
            &config,
            &ManualCompactionOptions::default(),
        )
        .await
        .expect("manual compaction should fall back to local");

        assert_eq!(mode, CompactionMode::Local);
        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nsummary");
    }

    #[tokio::test]
    async fn compact_history_manual_applies_options_to_local_summary_request() {
        let history = sample_history();
        let config = CompactionConfig {
            always_summarize: true,
            ..CompactionConfig::default()
        };
        let provider = CapturingProvider { last_request: Mutex::new(None) };
        let options = ManualCompactionOptions {
            instructions: Some("KEEP DECISIONS ONLY".to_string()),
            max_output_tokens: Some(128),
            reasoning_effort: Some(ReasoningEffortLevel::Minimal),
            verbosity: Some(VerbosityLevel::High),
        };

        let (compacted, mode) = compact_history_manual(&provider, "stub-model", &history, &config, &options)
            .await
            .expect("manual compaction");

        assert_eq!(mode, CompactionMode::Local);
        let captured = provider.last_request.lock().unwrap().clone().expect("captured summary request");
        assert_eq!(captured.max_tokens, Some(128));
        assert_eq!(captured.reasoning_effort, Some(ReasoningEffortLevel::Minimal));
        assert_eq!(captured.verbosity, Some(VerbosityLevel::High));
        // The custom instructions override the default summary prompt.
        let prompt = captured.messages[0].content.as_text();
        assert!(prompt.contains("KEEP DECISIONS ONLY"));
        assert!(!prompt.contains("acceptance criteria"));
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nsummary");
    }

    #[tokio::test]
    async fn compact_history_manual_returns_empty_for_empty_history() {
        let (compacted, mode) = compact_history_manual(
            &StubProvider,
            "stub-model",
            &[],
            &CompactionConfig::default(),
            &ManualCompactionOptions::default(),
        )
        .await
        .expect("manual compaction");

        assert!(compacted.is_empty());
        assert_eq!(mode, CompactionMode::Local);
    }

    #[tokio::test]
    async fn compact_history_rebuilds_history_around_summary_and_important_messages() {
        let history = vec![
            Message::assistant("setup".to_string()),
            Message::user("first request".to_string()),
            Message::assistant("working".to_string()),
            Message::tool_response("call-1".to_string(), "done".to_string()),
            Message::user("second request".to_string()),
            Message::assistant("final reply".to_string()),
        ];
        let config = CompactionConfig {
            always_summarize: true,
            ..CompactionConfig::default()
        };

        let compacted = compact_history(&StubProvider, "stub-model", &history, &config)
            .await
            .expect("compacted history");

        // Summary plus the complete newest protocol groups. The assistant/tool
        // messages remain paired with their user anchors in the continuity
        // tail.
        assert_eq!(compacted.len(), 6);
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nsummary");
        assert_eq!(compacted[1].content.as_text(), "first request");
        assert_eq!(compacted[2].content.as_text(), "working");
        assert_eq!(compacted[3].content.as_text(), "done");
        assert_eq!(compacted[4].content.as_text(), "second request");
        assert_eq!(compacted[5].content.as_text(), "final reply");
    }

    #[tokio::test]
    async fn compact_history_preserves_continuity_tail_over_retention_budget() {
        let history = vec![
            Message::user("alpha beta gamma delta epsilon zeta".to_string()),
            Message::assistant("ack".to_string()),
            Message::user("newest request".to_string()),
        ];
        let config = CompactionConfig {
            always_summarize: true,
            retained_user_message_tokens: 8,
            ..CompactionConfig::default()
        };

        let compacted = compact_history(&StubProvider, "stub-model", &history, &config)
            .await
            .expect("compacted history");

        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[1].content.as_text(), "alpha beta gamma delta epsilon zeta");
        assert_eq!(compacted[2].content.as_text(), "ack");
        assert_eq!(compacted[3].content.as_text(), "newest request");
    }

    #[tokio::test]
    async fn compacted_history_respects_model_context_budget() {
        let mut history = Vec::new();
        for index in 0..24 {
            history.push(Message::user(format!("request-{index} {}", "context ".repeat(1_200))));
            history.push(Message::assistant(format!("completed request {index}")));
        }
        let config = CompactionConfig {
            always_summarize: true,
            ..CompactionConfig::default()
        };

        let compacted = compact_history(&StubProvider, "stub-model", &history, &config)
            .await
            .expect("compacted history");
        let estimated_tokens = compacted.iter().map(Message::estimate_tokens).sum::<usize>();

        assert!(estimated_tokens <= 32_768 - 512);
    }

    #[tokio::test]
    async fn compact_history_caps_retained_user_message_count() {
        let history = vec![
            Message::user("first request".to_string()),
            Message::assistant("ack".to_string()),
            Message::user("second request".to_string()),
            Message::assistant("ack".to_string()),
            Message::user("third request".to_string()),
            Message::assistant("ack".to_string()),
            Message::user("fourth request".to_string()),
            Message::assistant("ack".to_string()),
            Message::user("fifth request".to_string()),
        ];
        let config = CompactionConfig {
            always_summarize: true,
            retained_user_messages: 4,
            ..CompactionConfig::default()
        };

        let compacted = compact_history(&StubProvider, "stub-model", &history, &config)
            .await
            .expect("compacted history");

        let continuity_tail = compacted
            .iter()
            .skip(1)
            .map(|message| message.content.as_text().to_string())
            .collect::<Vec<_>>();
        assert_eq!(continuity_tail.len(), history.len());
        assert_eq!(continuity_tail[0], "first request");
        assert_eq!(continuity_tail[8], "fifth request");
    }

    #[tokio::test]
    async fn compact_history_forces_local_summary_when_always_summarize_is_enabled() {
        let history = vec![
            Message::user("first request".to_string()),
            Message::assistant("working".to_string()),
            Message::user("second request".to_string()),
        ];
        let config = CompactionConfig {
            always_summarize: true,
            ..CompactionConfig::default()
        };

        let compacted = compact_history(&NativeCompactionProvider, "stub-model", &history, &config)
            .await
            .expect("compacted history");

        assert_eq!(compacted.len(), 4);
        assert_eq!(compacted[0].content.as_text(), "Previous conversation summary:\nsummary");
        assert_eq!(compacted[1].content.as_text(), "first request");
        assert_eq!(compacted[2].content.as_text(), "working");
        assert_eq!(compacted[3].content.as_text(), "second request");
    }

    #[test]
    fn default_summary_prompt_preserves_required_compaction_context() {
        let prompt = CompactionConfig::default().summary_prompt;

        assert!(prompt.contains("acceptance criteria"));
        assert!(prompt.contains("file paths that were read or modified"));
        assert!(prompt.contains("test results and error messages"));
        assert!(prompt.contains("decisions with their reasoning"));
    }

    #[test]
    fn continuity_tail_keeps_complete_turn_but_drops_unmatched_tool_call() {
        // A completed turn: user -> assistant(tool call) -> tool result. The
        // tail must keep the whole turn intact (the tool result makes the
        // trailing assistant tool call valid to send).
        let complete = vec![
            Message::user("do the thing".into()),
            {
                let mut m = Message::assistant("calling".into());
                m.tool_calls = Some(vec![ToolCall::function("c1".into(), "run".into(), "{}".into())]);
                m
            },
            Message::tool_response("c1".into(), "ran".into()),
        ];
        assert_eq!(continuity_tail(&complete).len(), 3);

        // An interrupted turn: user -> assistant(tool call) with no tool
        // result. Sending the trailing assistant message to a provider is
        // invalid, so the tail must drop it and keep only the user message.
        let interrupted = vec![Message::user("do the thing".into()), {
            let mut m = Message::assistant("calling".into());
            m.tool_calls = Some(vec![ToolCall::function("c1".into(), "run".into(), "{}".into())]);
            m
        }];
        let tail = continuity_tail(&interrupted);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].role, MessageRole::User);

        let parallel_complete = vec![
            Message::user("run both".into()),
            Message::assistant_with_tools(
                "calling".into(),
                vec![
                    ToolCall::function("c1".into(), "run".into(), "{}".into()),
                    ToolCall::function("c2".into(), "run".into(), "{}".into()),
                ],
            ),
            Message::tool_response("c1".into(), "first result".into()),
            Message::tool_response("c2".into(), "second result".into()),
        ];
        assert_eq!(continuity_tail(&parallel_complete).len(), 4);

        let parallel_interrupted = parallel_complete[..3].to_vec();
        let tail = continuity_tail(&parallel_interrupted);
        assert_eq!(tail.len(), 1, "a missing parallel result drops the whole assistant call");
        assert_eq!(tail[0].content.as_text(), "run both");
    }

    #[test]
    fn continuity_tail_keeps_newest_complete_groups_within_budget() {
        let mut history = Vec::new();
        for group_index in 0..12 {
            history.push(Message::user(format!("group-{group_index} {}", "alpha beta gamma delta ".repeat(2_000))));
            history.push(Message::assistant(format!("completed group {group_index}")));
        }

        let tail = continuity_tail(&history);
        let estimated_tokens = tail.iter().map(Message::estimate_tokens).sum::<usize>();

        assert!(estimated_tokens <= super::CONTINUITY_TAIL_TARGET_TOKENS);
        assert_eq!(tail.first().map(|message| message.role), Some(MessageRole::User));
        assert!(tail.iter().any(|message| message.content.as_text().contains("group-11")));
        assert!(!tail.iter().any(|message| message.content.as_text().contains("group-0 ")));
        assert_eq!(tail.len() % 2, 0, "protocol groups must remain atomic");
    }

    #[test]
    fn continuity_tail_preserves_a_user_anchor_larger_than_soft_budget() {
        let history = vec![
            Message::user("u".repeat(100_000)),
            Message::assistant("a".repeat(100_000)),
        ];

        let tail = continuity_tail(&history);

        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0], history[0], "the soft tail budget must never clip user instructions");
    }

    #[test]
    fn continuity_tail_bounds_tool_call_metadata_without_breaking_correlation() {
        let mut assistant = Message::assistant(String::new());
        assistant.tool_calls = Some(vec![ToolCall::function(
            "call-large".into(),
            "run_command".into(),
            format!("{{\"command\":\"{}\"}}", "x".repeat(300_000)),
        )]);
        let history = vec![
            Message::user("run the command".into()),
            assistant,
            Message::tool_response("call-large".into(), "completed".into()),
        ];

        let tail = continuity_tail(&history);
        assert!(tail.iter().map(Message::estimate_tokens).sum::<usize>() <= super::CONTINUITY_TAIL_TARGET_TOKENS);
        let call = tail[1].tool_calls.as_ref().expect("tool call should remain").first().unwrap();
        assert_eq!(call.id, "call-large");
        assert_eq!(call.function.as_ref().unwrap().arguments, "{}");
        assert_eq!(tail[2].tool_call_id.as_deref(), Some("call-large"));
    }

    #[test]
    fn provider_normalization_falls_back_to_source_tail_and_drops_pending_call() {
        let source = vec![
            Message::user("latest request".into()),
            Message::assistant("finished".into()),
        ];
        let provider_output = vec![Message::system("provider summary".into())];

        let normalized = super::normalize_provider_compacted_history(provider_output, &source);

        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[0].content.as_text(), "provider summary");
        assert_eq!(normalized[1].content.as_text(), "latest request");
        assert_eq!(normalized[2].content.as_text(), "finished");

        let malformed = vec![Message::system("summary".into()), {
            let mut message = Message::assistant("pending".into());
            message.tool_calls = Some(vec![ToolCall::function("c1".into(), "run".into(), "{}".into())]);
            message
        }];
        let normalized = super::normalize_provider_compacted_history(malformed, &source);
        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[0].content.as_text(), "summary");
        assert_eq!(normalized[1].content.as_text(), "latest request");

        let partial = vec![
            Message::system("provider summary".into()),
            Message::user("older request".into()),
            Message::assistant("older result".into()),
            Message::user("latest request".into()),
            Message::assistant_with_tools(
                "calling".into(),
                vec![
                    ToolCall::function("c1".into(), "run".into(), "{}".into()),
                    ToolCall::function("c2".into(), "run".into(), "{}".into()),
                ],
            ),
            Message::tool_response("c1".into(), "partial result".into()),
        ];
        let normalized = super::normalize_provider_compacted_history(partial, &source);
        assert_eq!(normalized[0].content.as_text(), "provider summary");
        assert_eq!(normalized.last().unwrap().content.as_text(), "finished");
        assert!(normalized.iter().any(|message| message.content.as_text() == "latest request"));
        assert!(!normalized.iter().any(|message| message.tool_call_id.as_deref() == Some("c1")));
        assert!(!normalized.iter().any(|message| message.tool_calls.is_some()));
    }

    #[test]
    fn provider_normalization_preserves_duplicate_tagged_intents() {
        use vtcode_commons::message_metadata::MessageMetadata;

        let mut first = Message::user("same follow-up".into());
        first.metadata = Some(MessageMetadata::user_input(1, first.estimate_tokens()).with_intent_id("intent-1"));
        let mut second = Message::user("same follow-up".into());
        second.metadata = Some(MessageMetadata::user_input(2, second.estimate_tokens()).with_intent_id("intent-2"));
        let source = vec![first, second];

        let normalized = super::normalize_provider_compacted_history(vec![Message::system("summary".into())], &source);
        let intent_ids = normalized
            .iter()
            .filter_map(|message| message.metadata.as_ref().and_then(|metadata| metadata.intent_id()))
            .collect::<Vec<_>>();

        assert_eq!(intent_ids, vec!["intent-1", "intent-2"]);
    }

    /// History-growth verify-item: the local summarization prompt must exclude
    /// `Message.reasoning` / `reasoning_details` and serialize only the visible
    /// text content (`content.as_text()`). Reasoning traces are large and
    /// ephemeral; including them in every compaction summary would bloat the
    /// post-compaction context and re-inject stale chain-of-thought. The
    /// continuity tail preserves provider-protocol reasoning (Anthropic
    /// thinking signatures, OpenAI reasoning items) separately and is NOT
    /// summarized, so stripping reasoning here is safe and correct. This test
    /// pins that invariant so a change to `build_summary_prompt` cannot
    /// accidentally start including reasoning.
    #[test]
    fn build_summary_prompt_excludes_reasoning_traces() {
        use super::build_summary_prompt;

        let instructions = "Summarize the conversation.";
        // Assistant message carrying a reasoning trace alongside its visible
        // content. If `build_summary_prompt` ever reads `reasoning`, the
        // summary would contain "SECRET_REASONING" and "raw-only reasoning".
        let history = vec![
            Message::user("What is 2+2?".to_string()),
            Message::assistant("The answer is 4.".to_string())
                .with_reasoning(Some("SECRET_REASONING: I computed 2+2=4.".to_string())),
        ];

        let prompt = build_summary_prompt(&history, instructions);

        assert!(prompt.contains("The answer is 4."), "visible assistant text must appear in the summary prompt");
        assert!(
            !prompt.contains("SECRET_REASONING"),
            "Message.reasoning must NOT be included in the summary prompt -- \
             it would bloat every compaction pass with ephemeral chain-of-thought"
        );
        assert!(prompt.contains("Summarize the conversation."), "instructions must appear in the summary prompt");
    }

    #[test]
    fn coherence_force_keeps_tool_results_not_in_selection() {
        // Regression: `coherence_tool_call_pairs` force-keeps the Tool results
        // that follow a retained Assistant-with-tool_calls, but the previous
        // implementation derived its output from `selected` only, so those
        // force-kept messages silently vanished. A compacted history can then
        // carry an Assistant tool-call with no result, which providers reject.
        use super::coherence_tool_call_pairs;

        let history = vec![
            Message::user("check the code".to_string()),
            Message::assistant("Looking...".to_string()).with_tool_calls(vec![ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: Some(crate::llm::provider::FunctionCall {
                    namespace: None,
                    name: "read_file".to_string(),
                    arguments: "{}".to_string(),
                }),
                text: None,
                thought_signature: None,
            }]),
            Message::tool_response("call_1".to_string(), "tool result that must survive".to_string()),
            Message::assistant("Done.".to_string()),
        ];

        // Selection keeps only the assistant-with-tool-calls turn, dropping the
        // following Tool result from the budget-driven selection.
        let selected = vec![(1, history[1].clone())];
        let result = coherence_tool_call_pairs(&history, &selected);

        let tool_results = result
            .iter()
            .filter(|(_, m)| m.role == MessageRole::Tool)
            .map(|(idx, m)| (*idx, m.content.as_text().to_string()))
            .collect::<Vec<_>>();

        assert_eq!(
            tool_results,
            vec![(2usize, "tool result that must survive".to_string())],
            "force-kept tool result must survive compaction even when not selected"
        );
        // History order must be preserved with the assistant preceding its result.
        let indices = result.iter().map(|(idx, _)| *idx).collect::<Vec<_>>();
        assert_eq!(indices, vec![1, 2]);
    }

    #[test]
    fn coherence_drops_orphaned_tool_results_of_unretained_assistant() {
        use super::coherence_tool_call_pairs;

        let history = vec![
            Message::user("check".to_string()),
            Message::assistant("Looking...".to_string()).with_tool_calls(vec![ToolCall {
                id: "call_1".to_string(),
                call_type: "function".to_string(),
                function: Some(crate::llm::provider::FunctionCall {
                    namespace: None,
                    name: "read_file".to_string(),
                    arguments: "{}".to_string(),
                }),
                text: None,
                thought_signature: None,
            }]),
            Message::tool_response("call_1".to_string(), "orphan result".to_string()),
        ];

        // The calling assistant was NOT retained; its orphaned Tool result must
        // be dropped so we never emit a result the model never saw a call for.
        let selected = vec![(0, history[0].clone())];
        let result = coherence_tool_call_pairs(&history, &selected);

        assert_eq!(result.len(), 1, "orphaned tool result must be dropped");
        assert_eq!(result[0].0, 0);
    }
}

#[cfg(test)]
mod prompt_preservation_tests;

#[cfg(test)]
mod property_tests;

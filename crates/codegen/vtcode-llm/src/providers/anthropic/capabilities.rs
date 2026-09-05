//! Model capability detection for Anthropic Claude models
//!
//! Provides methods to determine what features each Claude model supports:
//! - Reasoning/extended thinking
//! - Vision (image inputs)
//! - Structured outputs
//! - Parallel tool configuration
//! - Context window sizes

use crate::providers::anthropic_types::ThinkingDisplay;
use vtcode_config::constants::{models, reasoning};

const CLAUDE_OPUS_4_8: &str = "claude-opus-4-8";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClaudeThinkingProfile {
    pub supports_manual_budget: bool,
    pub adaptive_only: bool,
    pub default_thinking_enabled: bool,
    pub manual_interleaved_beta: bool,
    pub supports_effort: bool,
    pub supports_task_budget: bool,
    pub default_display: ThinkingDisplay,
    pub default_effort: &'static str,
    pub supports_xhigh_effort: bool,
    pub supports_max_effort: bool,
}

const ANTHROPIC_EFFORTS_UP_TO_HIGH: &[&str] = &[reasoning::LOW, reasoning::MEDIUM, reasoning::HIGH];
const ANTHROPIC_EFFORTS_UP_TO_MAX: &[&str] = &[reasoning::LOW, reasoning::MEDIUM, reasoning::HIGH, reasoning::MAX];
const ANTHROPIC_EFFORTS_UP_TO_XHIGH_AND_MAX: &[&str] = &[
    reasoning::LOW,
    reasoning::MEDIUM,
    reasoning::HIGH,
    reasoning::XHIGH,
    reasoning::MAX,
];

pub(crate) fn resolve_model_name<'a>(model: &'a str, default_model: &'a str) -> &'a str {
    if model.trim().is_empty() { default_model } else { model }
}

pub(crate) fn matches_model(model: &str, candidate: &str) -> bool {
    model == candidate || model.contains(candidate)
}

pub(crate) fn claude_thinking_profile(model: &str, default_model: &str) -> Option<ClaudeThinkingProfile> {
    let requested = resolve_model_name(model, default_model);

    // Check most specific models first – `matches_model` uses `contains`,
    // so `claude-fable-5-1` contains `claude-fable-5`. The 5.1 variants
    // must be checked before their 5 counterparts.
    if matches_model(requested, models::anthropic::CLAUDE_FABLE_5_1) {
        return Some(ClaudeThinkingProfile {
            supports_manual_budget: false,
            adaptive_only: true,
            default_thinking_enabled: true,
            manual_interleaved_beta: false,
            supports_effort: true,
            supports_task_budget: true,
            default_display: ThinkingDisplay::Omitted,
            default_effort: reasoning::HIGH,
            supports_xhigh_effort: true,
            supports_max_effort: true,
        });
    }

    if matches_model(requested, models::anthropic::CLAUDE_MYTHOS_5_1) {
        return Some(ClaudeThinkingProfile {
            supports_manual_budget: false,
            adaptive_only: true,
            default_thinking_enabled: true,
            manual_interleaved_beta: false,
            supports_effort: true,
            supports_task_budget: true,
            default_display: ThinkingDisplay::Omitted,
            default_effort: reasoning::HIGH,
            supports_xhigh_effort: true,
            supports_max_effort: true,
        });
    }

    if matches_model(requested, models::anthropic::CLAUDE_SONNET_5) {
        return Some(ClaudeThinkingProfile {
            supports_manual_budget: false,
            adaptive_only: false,
            default_thinking_enabled: true,
            manual_interleaved_beta: false,
            supports_effort: true,
            supports_task_budget: false,
            default_display: ThinkingDisplay::Omitted,
            default_effort: reasoning::HIGH,
            supports_xhigh_effort: true,
            supports_max_effort: true,
        });
    }

    if matches_model(requested, models::anthropic::CLAUDE_FABLE_5) {
        return Some(ClaudeThinkingProfile {
            supports_manual_budget: false,
            adaptive_only: true,
            default_thinking_enabled: true,
            manual_interleaved_beta: false,
            supports_effort: true,
            supports_task_budget: true,
            default_display: ThinkingDisplay::Omitted,
            default_effort: reasoning::HIGH,
            supports_xhigh_effort: true,
            supports_max_effort: true,
        });
    }

    if matches_model(requested, models::anthropic::CLAUDE_MYTHOS_5) {
        return Some(ClaudeThinkingProfile {
            supports_manual_budget: false,
            adaptive_only: true,
            default_thinking_enabled: true,
            manual_interleaved_beta: false,
            supports_effort: true,
            supports_task_budget: true,
            default_display: ThinkingDisplay::Omitted,
            default_effort: reasoning::HIGH,
            supports_xhigh_effort: true,
            supports_max_effort: true,
        });
    }

    if matches_model(requested, models::anthropic::CLAUDE_OPUS_5) {
        return Some(ClaudeThinkingProfile {
            supports_manual_budget: false,
            adaptive_only: false,
            default_thinking_enabled: true,
            manual_interleaved_beta: false,
            supports_effort: true,
            supports_task_budget: true,
            default_display: ThinkingDisplay::Omitted,
            default_effort: reasoning::HIGH,
            supports_xhigh_effort: true,
            supports_max_effort: true,
        });
    }

    None
}

fn supports_native_1m_context(model: &str) -> bool {
    matches_model(model, models::anthropic::CLAUDE_SONNET_5)
        || matches_model(model, models::anthropic::CLAUDE_FABLE_5)
        || matches_model(model, models::anthropic::CLAUDE_FABLE_5_1)
        || matches_model(model, models::anthropic::CLAUDE_MYTHOS_5)
        || matches_model(model, models::anthropic::CLAUDE_MYTHOS_5_1)
        || matches_model(model, models::anthropic::CLAUDE_OPUS_5)
}

pub(crate) fn supports_reasoning(model: &str, default_model: &str) -> bool {
    let requested = resolve_model_name(model, default_model);
    if claude_thinking_profile(requested, default_model).is_some() {
        return true;
    }

    models::minimax::SUPPORTED_MODELS.contains(&requested)
}

pub(crate) fn supports_reasoning_effort(model: &str, default_model: &str) -> bool {
    let requested = resolve_model_name(model, default_model);

    if claude_thinking_profile(requested, default_model).is_some() {
        return true;
    }

    if models::minimax::SUPPORTED_MODELS.contains(&requested) {
        return true;
    }

    models::anthropic::REASONING_MODELS.contains(&requested)
}

pub(crate) fn supports_effort(model: &str, default_model: &str) -> bool {
    claude_thinking_profile(model, default_model).is_some_and(|profile| profile.supports_effort)
}

pub(crate) fn supports_task_budget(model: &str, default_model: &str) -> bool {
    claude_thinking_profile(model, default_model).is_some_and(|profile| profile.supports_task_budget)
}

pub(crate) fn supports_manual_thinking_budget(model: &str, default_model: &str) -> bool {
    claude_thinking_profile(model, default_model).is_some_and(|profile| profile.supports_manual_budget)
}

pub(crate) fn supports_manual_interleaved_beta(model: &str, default_model: &str) -> bool {
    claude_thinking_profile(model, default_model).is_some_and(|profile| profile.manual_interleaved_beta)
}

pub(crate) fn supports_assistant_prefill(model: &str, default_model: &str) -> bool {
    let requested = resolve_model_name(model, default_model);

    // Current thinking-profile models do not support prefill. Models without a
    // thinking profile retain the legacy prefill fallback.
    claude_thinking_profile(requested, default_model).is_none()
}

pub(crate) fn supports_mid_conversation_system_messages(model: &str, default_model: &str) -> bool {
    supports_turn_scoped_system_messages(model, default_model)
}

/// Whether the model accepts Anthropic's turn-scoped `clear_at` system-message
/// field. The current beta is available to the model families that accept
/// mid-conversation system messages, but not Sonnet 5.
pub(crate) fn supports_turn_scoped_system_messages(model: &str, default_model: &str) -> bool {
    let requested = resolve_model_name(model, default_model);
    matches_model(requested, models::anthropic::CLAUDE_FABLE_5)
        || matches_model(requested, models::anthropic::CLAUDE_MYTHOS_5)
        || matches_model(requested, CLAUDE_OPUS_4_8)
        || matches_model(requested, models::anthropic::CLAUDE_OPUS_5)
}

pub(crate) fn adaptive_thinking_always_on(model: &str, default_model: &str) -> bool {
    claude_thinking_profile(model, default_model).is_some_and(|profile| profile.adaptive_only)
}

pub(crate) fn default_effort_for_model(model: &str, default_model: &str) -> Option<&'static str> {
    claude_thinking_profile(model, default_model)
        .filter(|profile| profile.supports_effort)
        .map(|profile| profile.default_effort)
}

pub(crate) fn allowed_efforts_for_model(model: &str, default_model: &str) -> Option<&'static [&'static str]> {
    let profile = claude_thinking_profile(model, default_model)?;
    if !profile.supports_effort {
        return None;
    }

    if profile.supports_xhigh_effort {
        Some(ANTHROPIC_EFFORTS_UP_TO_XHIGH_AND_MAX)
    } else if profile.supports_max_effort {
        Some(ANTHROPIC_EFFORTS_UP_TO_MAX)
    } else {
        Some(ANTHROPIC_EFFORTS_UP_TO_HIGH)
    }
}

pub(crate) fn effort_allowed_for_model(model: &str, default_model: &str, effort: &str) -> bool {
    let normalized = effort.trim().to_ascii_lowercase();
    allowed_efforts_for_model(model, default_model).is_some_and(|allowed| allowed.contains(&normalized.as_str()))
}

pub(crate) fn supports_compaction(model: &str) -> bool {
    matches_model(model, models::anthropic::CLAUDE_SONNET_5)
        || matches_model(model, models::anthropic::CLAUDE_FABLE_5)
        || matches_model(model, models::anthropic::CLAUDE_FABLE_5_1)
        || matches_model(model, models::anthropic::CLAUDE_MYTHOS_5)
        || matches_model(model, models::anthropic::CLAUDE_MYTHOS_5_1)
        || matches_model(model, models::anthropic::CLAUDE_OPUS_5)
        || matches_model(model, models::anthropic::CLAUDE_OPUS_5)
        || matches_model(model, models::anthropic::CLAUDE_SONNET_5)
}

pub(crate) fn supports_parallel_tool_config(_model: &str) -> bool {
    true
}

pub fn effective_context_size(model: &str) -> usize {
    if supports_native_1m_context(model) {
        1_000_000
    } else {
        200_000
    }
}

pub(crate) fn rejects_sampling(model: &str, default_model: &str) -> bool {
    let requested = resolve_model_name(model, default_model);
    matches_model(requested, models::anthropic::CLAUDE_SONNET_5)
        || matches_model(requested, models::anthropic::CLAUDE_FABLE_5)
        || matches_model(requested, models::anthropic::CLAUDE_FABLE_5_1)
        || matches_model(requested, models::anthropic::CLAUDE_MYTHOS_5)
        || matches_model(requested, models::anthropic::CLAUDE_MYTHOS_5_1)
        || matches_model(requested, models::anthropic::CLAUDE_OPUS_5)
        || matches_model(requested, models::anthropic::CLAUDE_OPUS_5)
}

pub(crate) fn supports_structured_output(model: &str, default_model: &str) -> bool {
    let requested = resolve_model_name(model, default_model);

    // All models with a thinking profile support structured outputs.
    if claude_thinking_profile(requested, default_model).is_some() {
        return true;
    }

    // Legacy models without thinking profiles that support structured outputs.
    matches_model(requested, "claude-sonnet-4-5")
        || matches_model(requested, "claude-opus-4-5")
        || matches_model(requested, "claude-sonnet-5")
}

pub(crate) fn supports_vision(model: &str, default_model: &str) -> bool {
    let requested = resolve_model_name(model, default_model);

    // All models with a thinking profile support vision.
    if claude_thinking_profile(requested, default_model).is_some() {
        return true;
    }

    // Legacy Claude 3 and Claude 4 Sonnet families support vision.
    requested.starts_with("claude-3") || requested.starts_with("claude-4-sonnet")
}

pub fn is_claude_model(model: &str, default_model: &str) -> bool {
    claude_thinking_profile(model, default_model).is_some()
}

pub(crate) fn supported_models() -> Vec<String> {
    let mut supported: Vec<String> = models::anthropic::SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect();

    supported.extend(models::minimax::SUPPORTED_MODELS.iter().map(|s| s.to_string()));

    supported.sort();
    supported.dedup();
    supported
}

/// Returns true if the effective effort for this request is "low", "medium", or "high"
/// (i.e., at most high, not xhigh or max).
///
/// This is used by Opus 5 disabled-thinking validation: Opus 5 only allows
/// `thinking: {type: "disabled"}` at effort ≤ high. When the override is `Omit`,
/// the API uses the model default effort, so we check that instead of the config.
pub(crate) fn effort_is_at_most_high(
    request: &crate::provider::LLMRequest,
    anthropic_config: &vtcode_config::core::AnthropicConfig,
) -> bool {
    use crate::provider::{AnthropicOptionalStringOverride, LLMRequest};
    use vtcode_config::types::ReasoningEffortLevel;

    if let Some(overrides) = request.anthropic_request_overrides.as_ref() {
        match &overrides.effort {
            AnthropicOptionalStringOverride::Explicit(effort) => {
                return matches!(effort.to_ascii_lowercase().as_str(), "low" | "medium" | "high");
            }
            AnthropicOptionalStringOverride::Omit => {
                return default_effort_for_model(&request.model, "").is_some_and(|effort| effort <= "high");
            }
            AnthropicOptionalStringOverride::Inherit => {}
        }
    }

    if let Some(effort) = request.effort.as_ref() {
        return matches!(effort.to_ascii_lowercase().as_str(), "low" | "medium" | "high");
    }
    if let Some(effort) = request.reasoning_effort {
        return matches!(effort, ReasoningEffortLevel::Low | ReasoningEffortLevel::Medium | ReasoningEffortLevel::High);
    }
    matches!(anthropic_config.effort.as_str(), "low" | "medium" | "high")
}

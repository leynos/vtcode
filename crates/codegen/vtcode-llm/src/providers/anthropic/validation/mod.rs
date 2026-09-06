//! Request validation for Anthropic Claude API
//!
//! Validates:
//! - Message requirements
//! - Structured output schema compliance
//! - Extended thinking parameter constraints

use crate::error_display;
use crate::provider::{
    AnthropicOptionalStringOverride, AnthropicOptionalU32Override, AnthropicThinkingModeOverride, LLMError, LLMRequest,
    MessageRole, ToolChoice,
};
use vtcode_config::core::AnthropicConfig;
use vtcode_config::types::ReasoningEffortLevel;

mod schema;
mod tool;

pub use schema::validate_anthropic_schema;
use tool::validate_tool_definitions;

use super::capabilities::{
    adaptive_thinking_always_on, allowed_efforts_for_model, claude_thinking_profile, default_effort_for_model,
    effort_allowed_for_model, effort_is_at_most_high, matches_model, rejects_sampling, resolve_model_name,
    supports_assistant_prefill, supports_effort, supports_manual_interleaved_beta, supports_manual_thinking_budget,
    supports_structured_output, supports_task_budget,
};

pub fn validate_request(
    request: &LLMRequest,
    default_model: &str,
    anthropic_config: &AnthropicConfig,
    provider_name: &str,
) -> Result<(), LLMError> {
    if request.messages.is_empty() {
        let formatted_error = error_display::format_llm_error(provider_name, "Messages cannot be empty");
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    // Note: Model name validation removed. The Anthropic API will validate model names
    // and return appropriate errors. This avoids maintenance burden of keeping hardcoded
    // model lists in sync and allows flexibility for proxies/aggregators.

    if request.output_format.is_some() && !supports_structured_output(&request.model, default_model) {
        let formatted_error = error_display::format_llm_error(
            provider_name,
            &format!(
                "Structured output is not supported for model '{}'. Structured outputs are only available for Claude Sonnet 4.5/4.6, Claude Opus 4.5/4.7/4.8, and Claude Haiku 4.5 models.",
                request.model
            ),
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    if let Some(ref schema) = request.output_format
        && supports_structured_output(&request.model, default_model)
    {
        validate_anthropic_schema(schema, provider_name)?;
    }

    if let Some(ref effort) = request.effort {
        validate_effort_setting(effort, &request.model, default_model)?;
    }

    let resolved_model = resolve_model_name(&request.model, default_model);
    let effective_thinking_mode = resolve_effective_thinking_mode(request, default_model, anthropic_config);

    // Models with adaptive thinking always on (Fable 5, Mythos 5) reject disabled thinking.
    // Sonnet 5 has default thinking on but allows disabling via `thinking: {type: "disabled"}`.
    // Opus 5 allows disabling thinking only at effort ≤ high.
    if adaptive_thinking_always_on(resolved_model, default_model)
        && matches!(effective_thinking_mode, EffectiveThinkingMode::Disabled)
    {
        let formatted_error = error_display::format_llm_error(
            provider_name,
            &format!(
                "{resolved_model} does not support disabled thinking on the Anthropic provider. Leave provider.anthropic.extended_thinking_enabled=true or choose another model."
            ),
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    if matches_model(resolved_model, vtcode_config::constants::models::anthropic::CLAUDE_OPUS_5)
        && matches!(effective_thinking_mode, EffectiveThinkingMode::Disabled)
    {
        if !effort_is_at_most_high(request, anthropic_config) {
            let formatted_error = error_display::format_llm_error(
                provider_name,
                "Claude Opus 5 does not support disabled thinking at xhigh or max effort. Lower effort to high or below, or remove the disable-thinking path.",
            );
            return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
        }
    }

    if rejects_sampling(&request.model, default_model)
        && (request.temperature.is_some() || request.top_p.is_some() || request.top_k.is_some())
    {
        let formatted_error = error_display::format_llm_error(
            provider_name,
            "Claude Opus 5, Sonnet 5, Fable 5, Mythos 5, and Opus 4.8 reject explicit temperature, top_p, and top_k values; omit sampling parameters entirely.",
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    if matches!(effective_thinking_mode, EffectiveThinkingMode::ManualBudget(_))
        && !supports_manual_thinking_budget(resolved_model, default_model)
    {
        let formatted_error = error_display::format_llm_error(
            provider_name,
            &format!(
                "{resolved_model} does not support thinking_budget/budget_tokens. Use adaptive thinking plus effort instead."
            ),
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    if request.thinking_budget.is_some()
        && claude_thinking_profile(resolved_model, default_model).is_some_and(|profile| !profile.supports_manual_budget)
    {
        let formatted_error = error_display::format_llm_error(
            provider_name,
            &format!(
                "{resolved_model} does not support thinking_budget/budget_tokens. Use adaptive thinking plus effort instead."
            ),
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    if let Some(budget) = effective_manual_thinking_budget_override(request)
        && budget < 1024
    {
        let formatted_error = error_display::format_llm_error(
            provider_name,
            &format!("thinking_budget ({budget}) must be at least 1024 tokens."),
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    let thinking_active = !matches!(effective_thinking_mode, EffectiveThinkingMode::Disabled);
    if thinking_active {
        validate_reasoning_constraints(request, default_model, anthropic_config)?;
    }

    // Prefill constraints only apply to models that support prefill.
    // For models that don't support prefill, the request builder silently omits it.
    if supports_assistant_prefill(resolved_model, default_model) {
        if request_uses_assistant_prefill(request) && thinking_active {
            let formatted_error = error_display::format_llm_error(
                provider_name,
                "Assistant-message prefills are not supported when thinking is enabled. Use system instructions instead.",
            );
            return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
        }

        if request_uses_assistant_prefill(request) && request.output_format.is_some() {
            let formatted_error = error_display::format_llm_error(
                provider_name,
                "Assistant-message prefills are not supported when structured outputs are enabled.",
            );
            return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
        }
    }

    if let Some(task_budget) = effective_task_budget_tokens(request, anthropic_config)
        && supports_task_budget(&request.model, default_model)
        && task_budget < 20_000
    {
        let formatted_error = error_display::format_llm_error(
            provider_name,
            &format!(
                "task_budget_tokens ({task_budget}) must be at least 20000 for Claude Opus 4.7/4.8, Fable 5, and Mythos 5."
            ),
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    if request.anthropic_request_overrides.is_some()
        && request.effort.is_some()
        && matches!(effective_thinking_mode, EffectiveThinkingMode::Disabled | EffectiveThinkingMode::ManualBudget(_))
    {
        let formatted_error = error_display::format_llm_error(
            provider_name,
            "output_config.effort is only valid for adaptive-thinking Anthropic requests.",
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    validate_tool_definitions(request)?;

    for message in request.messages.iter() {
        if let Err(err) = message.validate_for_provider("anthropic") {
            let formatted = error_display::format_llm_error(provider_name, &err);
            return Err(LLMError::InvalidRequest { message: formatted, metadata: None });
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectiveThinkingMode {
    Disabled,
    Adaptive,
    ManualBudget(u32),
}

fn resolve_effective_thinking_mode(
    request: &LLMRequest,
    default_model: &str,
    anthropic_config: &AnthropicConfig,
) -> EffectiveThinkingMode {
    let resolved_model = resolve_model_name(&request.model, default_model);
    if let Some(overrides) = request.anthropic_request_overrides.as_ref() {
        match overrides.thinking_mode {
            AnthropicThinkingModeOverride::Disabled => return EffectiveThinkingMode::Disabled,
            AnthropicThinkingModeOverride::Adaptive => return EffectiveThinkingMode::Adaptive,
            AnthropicThinkingModeOverride::ManualBudget(budget) => {
                return EffectiveThinkingMode::ManualBudget(budget);
            }
            AnthropicThinkingModeOverride::Inherit => {}
        }
    }

    let Some(profile) = claude_thinking_profile(resolved_model, default_model) else {
        if let Some(budget) = request.thinking_budget {
            return EffectiveThinkingMode::ManualBudget(budget);
        }
        if request
            .reasoning_effort
            .is_some_and(|effort| effort != ReasoningEffortLevel::None)
        {
            return EffectiveThinkingMode::Adaptive;
        }
        return EffectiveThinkingMode::Disabled;
    };

    if !anthropic_config.extended_thinking_enabled {
        return EffectiveThinkingMode::Disabled;
    }

    if profile.supports_manual_budget
        && let Some(budget) = request.thinking_budget
    {
        EffectiveThinkingMode::ManualBudget(budget)
    } else {
        EffectiveThinkingMode::Adaptive
    }
}

pub(crate) fn request_uses_assistant_prefill(request: &LLMRequest) -> bool {
    request.prefill.is_some()
        || request
            .coding_agent_settings
            .as_ref()
            .is_some_and(|settings| settings.prefill_thought)
        || (request.character_reinforcement && request.character_name.is_some())
}

fn effective_manual_thinking_budget_override(request: &LLMRequest) -> Option<u32> {
    if let Some(overrides) = request.anthropic_request_overrides.as_ref()
        && let AnthropicThinkingModeOverride::ManualBudget(budget) = overrides.thinking_mode
    {
        return Some(budget);
    }

    request.thinking_budget
}

fn effective_task_budget_tokens(request: &LLMRequest, anthropic_config: &AnthropicConfig) -> Option<u32> {
    if let Some(overrides) = request.anthropic_request_overrides.as_ref() {
        return match overrides.task_budget_tokens {
            AnthropicOptionalU32Override::Explicit(total) => Some(total),
            AnthropicOptionalU32Override::Omit => None,
            AnthropicOptionalU32Override::Inherit => anthropic_config.task_budget_tokens,
        };
    }

    anthropic_config.task_budget_tokens
}

fn validate_effort_setting(effort: &str, model: &str, default_model: &str) -> Result<(), LLMError> {
    let normalized = effort.trim().to_ascii_lowercase();
    let is_supported = supports_effort(model, default_model);

    if !is_supported {
        let formatted_error = error_display::format_llm_error(
            "Anthropic",
            &format!(
                "effort is not supported for model '{}'.",
                if model.trim().is_empty() { default_model } else { model }
            ),
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    let allowed = allowed_efforts_for_model(model, default_model).unwrap_or(&[]);
    if !effort_allowed_for_model(model, default_model, &normalized) {
        let formatted_error = error_display::format_llm_error(
            "Anthropic",
            &format!("effort must be one of {} (got '{}').", allowed.join(", "), effort,),
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    Ok(())
}

fn validate_reasoning_constraints(
    request: &LLMRequest,
    default_model: &str,
    anthropic_config: &AnthropicConfig,
) -> Result<(), LLMError> {
    if let Some(ToolChoice::Any | ToolChoice::Specific(_)) = request.tool_choice {
        let formatted_error = error_display::format_llm_error(
            "Anthropic",
            "Forced tool use (any/specific) is incompatible with extended thinking. Use 'auto' or 'none'.",
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    if let EffectiveThinkingMode::ManualBudget(budget) =
        resolve_effective_thinking_mode(request, default_model, anthropic_config)
    {
        let max_tokens = request.max_tokens.unwrap_or(4096);
        if supports_manual_thinking_budget(&request.model, default_model)
            && budget >= max_tokens
            && !supports_manual_interleaved_beta(&request.model, default_model)
        {
            let formatted_error = error_display::format_llm_error(
                "Anthropic",
                &format!(
                    "The value of max_tokens ({max_tokens}) must be strictly greater than budget_tokens ({budget}) when extended thinking is enabled without interleaved-thinking support."
                ),
            );
            return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
        }

        if request.temperature.is_some() || request.top_k.is_some() {
            let formatted_error = error_display::format_llm_error(
                "Anthropic",
                "temperature and top_k parameters must not be set when extended thinking is enabled.",
            );
            return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
        }
    }

    if let Some(top_p) = request.top_p
        && !(0.95..=1.0).contains(&top_p)
    {
        let formatted_error = error_display::format_llm_error(
            "Anthropic",
            &format!("top_p must be between 0.95 and 1.0 (got {top_p}) when extended thinking is enabled."),
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    if let Some(last_msg) = request.messages.last()
        && last_msg.role == MessageRole::Assistant
    {
        let formatted_error = error_display::format_llm_error(
            "Anthropic",
            "Pre-filling assistant responses is not supported when extended thinking is enabled.",
        );
        return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
    }

    Ok(())
}

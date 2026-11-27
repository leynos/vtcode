//! Sparse per-provider/per-model capability profiles and their resolved form.
use serde::{Deserialize, Serialize};

use crate::types::ReasoningEffortLevel;

use super::api_format::{CustomProviderApiFormat, skip_serializing_custom_provider_api_format};

/// Sparse per-provider or per-model capability/profile settings.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct CustomProviderProfileConfig {
    /// Typed API format for this provider/profile.
    #[serde(default, skip_serializing_if = "skip_serializing_custom_provider_api_format")]
    pub api_format: CustomProviderApiFormat,

    /// Optional context window size in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,

    /// Optional sampling temperature override (0.0-2.0) sent with requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Optional nucleus-sampling override (0.0-1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    /// Optional top-k override (>= 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i32>,

    /// Optional presence penalty override (-2.0-2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,

    /// Optional frequency penalty override (-2.0-2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,

    /// Optional max output tokens override (> 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Optional reasoning effort override sent with requests for this model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortLevel>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_effort: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_structured_output: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_parallel_tool_calls: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_context_caching: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_responses_compaction: Option<bool>,

    /// Whether streaming requests should ask the provider to return usage.
    ///
    /// The opt-in applies only to custom providers using the OpenAI Chat
    /// Completions API format. A profile value overrides the provider default,
    /// so an omitted value inherits it. Native OpenAI streams are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_stream_usage: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_context_edits: Option<bool>,
}

impl CustomProviderProfileConfig {
    pub(super) fn validate(&self, provider_name: &str, profile_key: &str) -> Result<(), String> {
        if self.context_window == Some(0) {
            return Err(format!(
                "custom_providers[{provider_name}].profiles[{profile_key}]: `context_window` must be greater than 0"
            ));
        }

        if let Some(temperature) = self.temperature
            && !(0.0..=2.0).contains(&temperature)
        {
            return Err(format!(
                "custom_providers[{provider_name}].profiles[{profile_key}]: `temperature` must be between 0.0 and 2.0"
            ));
        }

        if let Some(top_p) = self.top_p
            && !(0.0..=1.0).contains(&top_p)
        {
            return Err(format!(
                "custom_providers[{provider_name}].profiles[{profile_key}]: `top_p` must be between 0.0 and 1.0"
            ));
        }

        if let Some(top_k) = self.top_k
            && top_k < 0
        {
            return Err(format!("custom_providers[{provider_name}].profiles[{profile_key}]: `top_k` must be >= 0"));
        }

        for (field, value) in [
            ("`presence_penalty`", self.presence_penalty),
            ("`frequency_penalty`", self.frequency_penalty),
        ] {
            if let Some(value) = value
                && !(-2.0..=2.0).contains(&value)
            {
                return Err(format!(
                    "custom_providers[{provider_name}].profiles[{profile_key}]: {field} must be between -2.0 and 2.0"
                ));
            }
        }

        if self.max_tokens == Some(0) {
            return Err(format!(
                "custom_providers[{provider_name}].profiles[{profile_key}]: `max_tokens` must be greater than 0"
            ));
        }

        if self.reasoning_effort == Some(ReasoningEffortLevel::Unknown) {
            return Err(format!(
                "custom_providers[{provider_name}].profiles[{profile_key}]: `reasoning_effort` is not a recognized level (use none, minimal, low, medium, high, xhigh, or max)"
            ));
        }

        Ok(())
    }
}

/// Resolved capability/profile settings after applying provider defaults and
/// exact model-specific overrides.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedCustomProviderProfile {
    pub api_format: Option<CustomProviderApiFormat>,
    pub context_window: Option<usize>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<i32>,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
    pub max_tokens: Option<u32>,
    pub reasoning_effort: Option<ReasoningEffortLevel>,
    pub supports_tools: Option<bool>,
    pub supports_reasoning: Option<bool>,
    pub supports_reasoning_effort: Option<bool>,
    pub supports_vision: Option<bool>,
    pub supports_structured_output: Option<bool>,
    pub supports_parallel_tool_calls: Option<bool>,
    pub supports_context_caching: Option<bool>,
    pub supports_responses_compaction: Option<bool>,
    pub supports_stream_usage: Option<bool>,
    pub supports_context_edits: Option<bool>,
}

impl ResolvedCustomProviderProfile {
    pub(super) fn from_layers(
        defaults: &CustomProviderProfileConfig,
        profile: Option<&CustomProviderProfileConfig>,
    ) -> Self {
        let fallback_profile;
        let profile = match profile {
            Some(profile) => profile,
            None => {
                fallback_profile = CustomProviderProfileConfig::default();
                &fallback_profile
            }
        };

        Self {
            api_format: profile.api_format.resolved().or(defaults.api_format.resolved()),
            context_window: profile.context_window.or(defaults.context_window),
            temperature: profile.temperature.or(defaults.temperature),
            top_p: profile.top_p.or(defaults.top_p),
            top_k: profile.top_k.or(defaults.top_k),
            presence_penalty: profile.presence_penalty.or(defaults.presence_penalty),
            frequency_penalty: profile.frequency_penalty.or(defaults.frequency_penalty),
            max_tokens: profile.max_tokens.or(defaults.max_tokens),
            reasoning_effort: profile.reasoning_effort.or(defaults.reasoning_effort),
            supports_tools: profile.supports_tools.or(defaults.supports_tools),
            supports_reasoning: profile.supports_reasoning.or(defaults.supports_reasoning),
            supports_reasoning_effort: profile.supports_reasoning_effort.or(defaults.supports_reasoning_effort),
            supports_vision: profile.supports_vision.or(defaults.supports_vision),
            supports_structured_output: profile.supports_structured_output.or(defaults.supports_structured_output),
            supports_parallel_tool_calls: profile
                .supports_parallel_tool_calls
                .or(defaults.supports_parallel_tool_calls),
            supports_context_caching: profile.supports_context_caching.or(defaults.supports_context_caching),
            supports_responses_compaction: profile
                .supports_responses_compaction
                .or(defaults.supports_responses_compaction),
            supports_stream_usage: profile.supports_stream_usage.or(defaults.supports_stream_usage),
            supports_context_edits: profile.supports_context_edits.or(defaults.supports_context_edits),
        }
    }
}

//! Configuration for user-defined OpenAI-compatible provider endpoints.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::types::ReasoningEffortLevel;

use super::api_format::{CustomProviderApiFormat, skip_serializing_custom_provider_api_format};
use super::auth::CustomProviderCommandAuthConfig;
use super::profile::{CustomProviderProfileConfig, ResolvedCustomProviderProfile};
use super::request_policy::CustomProviderRequestPolicyConfig;

mod validation;

/// Configuration for a user-defined OpenAI-compatible provider endpoint.
///
/// Allows users to define multiple named custom endpoints (e.g., corporate
/// proxies) with distinct display names, so they can toggle between them
/// and clearly see which endpoint is active.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct CustomProviderConfig {
    /// Stable provider key used for routing and persistence (e.g., "mycorp").
    /// Must be lowercase alphanumeric with optional hyphens/underscores.
    pub name: String,

    /// Human-friendly label shown in the TUI header, footer, and model picker
    /// (e.g., "MyCorporateName").
    pub display_name: String,

    /// Base URL of the OpenAI-compatible API endpoint
    /// (e.g., `<https://llm.corp.example/v1>`). Non-empty custom providers
    /// from repository-controlled workspace/project layers are rejected.
    pub base_url: String,

    /// Typed API format for the provider's default profile.
    #[serde(default, skip_serializing_if = "skip_serializing_custom_provider_api_format")]
    pub api_format: CustomProviderApiFormat,

    /// Optional context window size in tokens for models served by this endpoint.
    ///
    /// When omitted, the OpenAI-compatible provider uses its default context
    /// window size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,

    /// Optional support for tool calling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tools: Option<bool>,

    /// Optional support for reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning: Option<bool>,

    /// Optional support for reasoning effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_reasoning_effort: Option<bool>,

    /// Optional support for vision inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,

    /// Optional support for structured output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_structured_output: Option<bool>,

    /// Optional support for parallel tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_parallel_tool_calls: Option<bool>,

    /// Optional support for context caching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_context_caching: Option<bool>,

    /// Optional support for responses compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_responses_compaction: Option<bool>,

    /// Optional support for streamed usage chunks.
    ///
    /// The opt-in applies only to custom providers using the OpenAI Chat
    /// Completions API format. A profile value overrides this default, so an
    /// omitted profile value inherits it. Native OpenAI streams are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_stream_usage: Option<bool>,

    /// Optional support for context edits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_context_edits: Option<bool>,

    /// Optional sampling temperature default (0.0-2.0) for models served by
    /// this endpoint unless a profile overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    /// Optional nucleus-sampling default (0.0-1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    /// Optional top-k default (>= 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<i32>,

    /// Optional presence penalty default (-2.0-2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,

    /// Optional frequency penalty default (-2.0-2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,

    /// Optional max output tokens default (> 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Optional reasoning effort default for models without a profile
    /// override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortLevel>,

    /// Environment variable name that holds the API key for this endpoint
    /// (e.g., "MYCORP_API_KEY").
    #[serde(default)]
    pub api_key_env: String,

    /// Optional command-backed bearer token configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<CustomProviderCommandAuthConfig>,

    /// Default model to use with this endpoint (e.g., "gpt-5-mini").
    ///
    /// When [`models`](Self::models) is empty, this single model is what the
    /// `/model` picker offers for this provider. When [`models`](Self::models)
    /// is non-empty, this field is used as the default selection but the
    /// picker lists every entry in [`models`](Self::models).
    #[serde(default)]
    pub model: String,

    /// Optional list of additional model identifiers offered by the provider.
    ///
    /// Useful for OpenAI-compatible aggregators such as Atlas Cloud that
    /// expose many models behind a single endpoint. When set, the `/model`
    /// picker shows one entry per model. When empty, the picker falls back to
    /// the single [`model`](Self::model) field.
    #[serde(default)]
    pub models: Vec<String>,

    /// Exact model-keyed sparse capability profiles.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, CustomProviderProfileConfig>,

    /// Per-process request admission and transient retry policy.
    #[serde(default)]
    pub request_policy: CustomProviderRequestPolicyConfig,
}

impl CustomProviderConfig {
    /// Resolve the API key environment variable used for this provider.
    ///
    /// Falls back to a derived `NAME_API_KEY`-style variable when the config
    /// does not set `api_key_env`.
    pub fn resolved_api_key_env(&self) -> String {
        if !self.api_key_env.trim().is_empty() {
            return self.api_key_env.clone();
        }

        crate::api_keys::api_key_env_var(&self.name)
    }

    pub fn uses_command_auth(&self) -> bool {
        self.auth.is_some()
    }

    /// Return the list of models the `/model` picker should offer for this
    /// provider.
    ///
    /// If `models` is non-empty, every entry is returned (trimmed). Otherwise
    /// the single `model` field is returned as a one-element list. An empty
    /// `model` field with no `models` list yields an empty result.
    pub fn effective_models(&self) -> Vec<String> {
        if !self.models.is_empty() {
            return self
                .models
                .iter()
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect();
        }
        let trimmed = self.model.trim();
        if trimmed.is_empty() {
            Vec::new()
        } else {
            vec![trimmed.to_string()]
        }
    }

    pub fn profile(&self, model: &str) -> Option<&CustomProviderProfileConfig> {
        self.profiles.get(model)
    }

    pub fn resolved_profile(&self, model: &str) -> ResolvedCustomProviderProfile {
        let defaults = self.provider_defaults_profile();
        ResolvedCustomProviderProfile::from_layers(&defaults, self.profile(model))
    }

    pub fn provider_defaults_profile(&self) -> CustomProviderProfileConfig {
        CustomProviderProfileConfig {
            api_format: self.api_format,
            context_window: self.context_window,
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            presence_penalty: self.presence_penalty,
            frequency_penalty: self.frequency_penalty,
            max_tokens: self.max_tokens,
            reasoning_effort: self.reasoning_effort,
            supports_tools: self.supports_tools,
            supports_reasoning: self.supports_reasoning,
            supports_reasoning_effort: self.supports_reasoning_effort,
            supports_vision: self.supports_vision,
            supports_structured_output: self.supports_structured_output,
            supports_parallel_tool_calls: self.supports_parallel_tool_calls,
            supports_context_caching: self.supports_context_caching,
            supports_responses_compaction: self.supports_responses_compaction,
            supports_stream_usage: self.supports_stream_usage,
            supports_context_edits: self.supports_context_edits,
        }
    }
}

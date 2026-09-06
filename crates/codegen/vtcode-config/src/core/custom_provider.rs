use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::models::ModelPricing;
use crate::types::ReasoningEffortLevel;

fn default_auth_timeout_ms() -> u64 {
    5_000
}

fn default_auth_refresh_interval_ms() -> u64 {
    300_000
}

fn skip_serializing_custom_provider_api_format(api_format: &CustomProviderApiFormat) -> bool {
    api_format.is_auto()
}

/// Semantic mapping from provider response metadata to rate-limit headers.
///
/// Header names are configurable because OpenAI-compatible providers expose
/// equivalent quota information under different names. The default mapping
/// covers the four Baseten/OpenAI-style per-minute headers.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct RateLimitHeaderConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_limit_per_minute: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_remaining_per_minute: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_limit_per_minute: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_remaining_per_minute: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_limit_per_second: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_remaining_per_second: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_limit_per_second: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_remaining_per_second: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_limit_per_second: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_adjusted_prompt_tokens_limit_per_second: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_tokens_limit_per_second: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_prompt_tokens: Option<String>,
    /// Header containing a provider-suggested reset interval in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_after_seconds: Option<String>,
}

impl Default for RateLimitHeaderConfig {
    fn default() -> Self {
        Self {
            requests_limit_per_minute: Some("x-ratelimit-limit-requests".to_string()),
            requests_remaining_per_minute: Some("x-ratelimit-remaining-requests".to_string()),
            tokens_limit_per_minute: Some("x-ratelimit-limit-tokens".to_string()),
            tokens_remaining_per_minute: Some("x-ratelimit-remaining-tokens".to_string()),
            requests_limit_per_second: None,
            requests_remaining_per_second: None,
            tokens_limit_per_second: None,
            tokens_remaining_per_second: None,
            prompt_tokens_limit_per_second: None,
            cache_adjusted_prompt_tokens_limit_per_second: None,
            generated_tokens_limit_per_second: None,
            prompt_tokens: None,
            cached_prompt_tokens: None,
            reset_after_seconds: None,
        }
    }
}

impl RateLimitHeaderConfig {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }

    fn fireworks_defaults() -> Self {
        Self {
            prompt_tokens_limit_per_second: Some("x-ratelimit-limit-tokens-prompt".to_string()),
            cache_adjusted_prompt_tokens_limit_per_second: Some(
                "x-ratelimit-limit-tokens-cache-adjusted-prompt".to_string(),
            ),
            generated_tokens_limit_per_second: Some("x-ratelimit-limit-tokens-generated".to_string()),
            prompt_tokens: Some("fireworks-prompt-tokens".to_string()),
            cached_prompt_tokens: Some("fireworks-cached-prompt-tokens".to_string()),
            ..Self::default()
        }
    }

    fn together_defaults() -> Self {
        Self {
            requests_limit_per_second: Some("x-ratelimit-limit".to_string()),
            requests_remaining_per_second: Some("x-ratelimit-remaining".to_string()),
            tokens_limit_per_second: Some("x-tokenlimit-limit".to_string()),
            tokens_remaining_per_second: Some("x-tokenlimit-remaining".to_string()),
            reset_after_seconds: Some("x-ratelimit-reset".to_string()),
            ..Self::default()
        }
    }

    /// Return default mappings for a provider name.
    ///
    /// The four per-minute mappings are universal custom-provider defaults;
    /// recognized Fireworks and Together aliases add their documented fields.
    pub fn for_provider_name(provider_name: &str) -> Self {
        let normalized_name = provider_name.to_ascii_lowercase();
        if normalized_name.contains("fireworks") {
            Self::fireworks_defaults()
        } else if normalized_name.contains("together") {
            Self::together_defaults()
        } else {
            Self::default()
        }
    }

    fn fill_missing_from(&mut self, defaults: &Self) {
        macro_rules! fill_missing {
            ($($field:ident),+ $(,)?) => {
                $(
                    if self.$field.is_none() {
                        self.$field.clone_from(&defaults.$field);
                    }
                )+
            };
        }

        fill_missing!(
            requests_limit_per_minute,
            requests_remaining_per_minute,
            tokens_limit_per_minute,
            tokens_remaining_per_minute,
            requests_limit_per_second,
            requests_remaining_per_second,
            tokens_limit_per_second,
            tokens_remaining_per_second,
            prompt_tokens_limit_per_second,
            cache_adjusted_prompt_tokens_limit_per_second,
            generated_tokens_limit_per_second,
            prompt_tokens,
            cached_prompt_tokens,
            reset_after_seconds,
        );
    }

    fn validate(&self, provider_name: &str) -> Result<(), String> {
        let headers = [
            ("requests_limit_per_minute", &self.requests_limit_per_minute),
            ("requests_remaining_per_minute", &self.requests_remaining_per_minute),
            ("tokens_limit_per_minute", &self.tokens_limit_per_minute),
            ("tokens_remaining_per_minute", &self.tokens_remaining_per_minute),
            ("requests_limit_per_second", &self.requests_limit_per_second),
            ("requests_remaining_per_second", &self.requests_remaining_per_second),
            ("tokens_limit_per_second", &self.tokens_limit_per_second),
            ("tokens_remaining_per_second", &self.tokens_remaining_per_second),
            ("prompt_tokens_limit_per_second", &self.prompt_tokens_limit_per_second),
            ("cache_adjusted_prompt_tokens_limit_per_second", &self.cache_adjusted_prompt_tokens_limit_per_second),
            ("generated_tokens_limit_per_second", &self.generated_tokens_limit_per_second),
            ("prompt_tokens", &self.prompt_tokens),
            ("cached_prompt_tokens", &self.cached_prompt_tokens),
            ("reset_after_seconds", &self.reset_after_seconds),
        ];

        for (field, header) in headers {
            if header.as_deref().is_some_and(|header| !is_valid_header_name(header)) {
                return Err(format!(
                    "custom_providers[{provider_name}].rate_limit_headers: `{field}` must be a valid HTTP header name"
                ));
            }
        }
        Ok(())
    }
}

fn is_valid_header_name(header: &str) -> bool {
    !header.is_empty()
        && header.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// Typed API format used by custom providers.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum CustomProviderApiFormat {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "openai-chat")]
    OpenAIChat,
    #[serde(rename = "openai-responses")]
    OpenAIResponses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
}

/// Optional per-token pricing for a custom provider model.
///
/// Values are configured in USD per million tokens and converted to the
/// per-token representation used by the runtime cost estimator.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq)]
pub struct CustomProviderPricingConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_per_million_usd: Option<f64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_per_million_usd: Option<f64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_per_million_usd: Option<f64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_per_million_usd: Option<f64>,
}

impl CustomProviderPricingConfig {
    const TOKENS_PER_MILLION: f64 = 1_000_000.0;

    fn from_layers(defaults: Self, profile: Self) -> Self {
        Self {
            input_per_million_usd: profile.input_per_million_usd.or(defaults.input_per_million_usd),
            output_per_million_usd: profile.output_per_million_usd.or(defaults.output_per_million_usd),
            cache_read_per_million_usd: profile.cache_read_per_million_usd.or(defaults.cache_read_per_million_usd),
            cache_write_per_million_usd: profile.cache_write_per_million_usd.or(defaults.cache_write_per_million_usd),
        }
    }

    pub fn model_pricing(self) -> Option<ModelPricing> {
        let input = self.input_per_million_usd? / Self::TOKENS_PER_MILLION;
        let output = self.output_per_million_usd? / Self::TOKENS_PER_MILLION;
        Some(ModelPricing {
            input: Some(input),
            output: Some(output),
            cache_read: self.cache_read_per_million_usd.map(|rate| rate / Self::TOKENS_PER_MILLION),
            cache_write: self.cache_write_per_million_usd.map(|rate| rate / Self::TOKENS_PER_MILLION),
        })
    }

    fn validate(self, subject: &str) -> Result<(), String> {
        let rates = [
            ("input_per_million_usd", self.input_per_million_usd),
            ("output_per_million_usd", self.output_per_million_usd),
            ("cache_read_per_million_usd", self.cache_read_per_million_usd),
            ("cache_write_per_million_usd", self.cache_write_per_million_usd),
        ];
        for (name, rate) in rates {
            if rate.is_some_and(|rate| !rate.is_finite() || rate < 0.0) {
                return Err(format!("{subject}.pricing: `{name}` must be a finite non-negative number"));
            }
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.input_per_million_usd.is_none()
            && self.output_per_million_usd.is_none()
            && self.cache_read_per_million_usd.is_none()
            && self.cache_write_per_million_usd.is_none()
    }
}

impl CustomProviderApiFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::OpenAIChat => "openai-chat",
            Self::OpenAIResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }

    pub const fn is_auto(self) -> bool {
        matches!(self, Self::Auto)
    }

    pub const fn resolved(self) -> Option<Self> {
        match self {
            Self::Auto => None,
            other => Some(other),
        }
    }
}

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

    /// Optional pricing override for this exact model profile.
    #[serde(default, skip_serializing_if = "CustomProviderPricingConfig::is_empty")]
    pub pricing: CustomProviderPricingConfig,

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_stream_usage: Option<bool>,

    /// Allow terminal Responses function-call IDs to replace mismatched
    /// streamed IDs after strict semantic one-to-one validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responses_allow_function_call_id_remap: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_context_edits: Option<bool>,
}

impl CustomProviderProfileConfig {
    fn validate(&self, provider_name: &str, profile_key: &str) -> Result<(), String> {
        if self.context_window == Some(0) {
            return Err(format!(
                "custom_providers[{provider_name}].profiles[{profile_key}]: `context_window` must be greater than 0"
            ));
        }

        self.pricing
            .validate(&format!("custom_providers[{provider_name}].profiles[{profile_key}]"))?;

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
    pub pricing: CustomProviderPricingConfig,
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
    pub responses_allow_function_call_id_remap: Option<bool>,
    pub supports_context_edits: Option<bool>,
}

impl ResolvedCustomProviderProfile {
    fn from_layers(defaults: &CustomProviderProfileConfig, profile: Option<&CustomProviderProfileConfig>) -> Self {
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
            pricing: CustomProviderPricingConfig::from_layers(defaults.pricing, profile.pricing),
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
            responses_allow_function_call_id_remap: profile
                .responses_allow_function_call_id_remap
                .or(defaults.responses_allow_function_call_id_remap),
            supports_context_edits: profile.supports_context_edits.or(defaults.supports_context_edits),
        }
    }
}

/// Command-backed bearer token configuration for a custom provider.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CustomProviderCommandAuthConfig {
    /// Command to execute. Bare names are resolved via `PATH`. Command-backed
    /// auth is accepted only from trusted system/user or explicitly selected
    /// configuration; repository-controlled workspace/project values are
    /// rejected.
    pub command: String,

    /// Optional command arguments.
    #[serde(default)]
    pub args: Vec<String>,

    /// Optional working directory for the token command.
    #[serde(default)]
    pub cwd: Option<PathBuf>,

    /// Maximum time to wait for the command to complete successfully.
    #[serde(default = "default_auth_timeout_ms")]
    pub timeout_ms: u64,

    /// Maximum age for the cached token before rerunning the command.
    #[serde(default = "default_auth_refresh_interval_ms")]
    pub refresh_interval_ms: u64,
}

impl Default for CustomProviderCommandAuthConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            cwd: None,
            timeout_ms: default_auth_timeout_ms(),
            refresh_interval_ms: default_auth_refresh_interval_ms(),
        }
    }
}

impl CustomProviderCommandAuthConfig {
    fn validate(&self, provider_name: &str) -> Result<(), String> {
        if self.command.trim().is_empty() {
            return Err(format!("custom_providers[{provider_name}]: `auth.command` must not be empty"));
        }

        if self.timeout_ms == 0 {
            return Err(format!("custom_providers[{provider_name}]: `auth.timeout_ms` must be greater than 0"));
        }

        if self.refresh_interval_ms == 0 {
            return Err(format!(
                "custom_providers[{provider_name}]: `auth.refresh_interval_ms` must be greater than 0"
            ));
        }

        Ok(())
    }
}

const fn default_provider_queue_timeout_seconds() -> u64 {
    600
}

const fn default_provider_max_retries() -> u32 {
    2
}

const fn default_provider_retry_initial_backoff_ms() -> u64 {
    10_000
}

const fn default_provider_retry_max_backoff_ms() -> u64 {
    160_000
}

const fn default_provider_retry_jitter() -> bool {
    true
}

const fn default_provider_connect_timeout_seconds() -> u64 {
    30
}

const fn default_provider_first_token_timeout_seconds() -> u64 {
    180
}

const fn default_provider_stream_idle_timeout_seconds() -> u64 {
    120
}

const fn default_provider_total_generation_timeout_seconds() -> u64 {
    600
}

/// Runtime admission and retry policy for a custom provider.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct CustomProviderRequestPolicyConfig {
    /// Maximum provider requests that one VT Code process may keep in flight.
    /// `None` leaves concurrency unrestricted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_in_flight_requests: Option<usize>,

    /// Maximum time to wait for an in-process provider permit.
    #[serde(default = "default_provider_queue_timeout_seconds")]
    pub queue_timeout_seconds: u64,

    /// Number of retries after the initial request for transient failures.
    #[serde(default = "default_provider_max_retries")]
    pub max_retries: u32,

    /// Initial retry backoff duration in milliseconds.
    #[serde(default = "default_provider_retry_initial_backoff_ms")]
    pub retry_initial_backoff_ms: u64,

    /// Maximum retry backoff duration in milliseconds.
    #[serde(default = "default_provider_retry_max_backoff_ms")]
    pub retry_max_backoff_ms: u64,

    /// Add deterministic jitter to retry delays to avoid synchronized reconnects.
    #[serde(default = "default_provider_retry_jitter")]
    pub retry_jitter: bool,

    /// Maximum time to establish the provider connection. Zero disables the limit.
    #[serde(default = "default_provider_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,

    /// Maximum time to wait for the first streamed event. Zero disables the limit.
    #[serde(default = "default_provider_first_token_timeout_seconds")]
    pub first_token_timeout_seconds: u64,

    /// Maximum time between streamed events. Zero disables the limit.
    #[serde(default = "default_provider_stream_idle_timeout_seconds")]
    pub stream_idle_timeout_seconds: u64,

    /// Maximum duration of one provider generation attempt. Zero disables the limit.
    #[serde(default = "default_provider_total_generation_timeout_seconds")]
    pub total_generation_timeout_seconds: u64,
}

impl Default for CustomProviderRequestPolicyConfig {
    fn default() -> Self {
        Self {
            max_in_flight_requests: None,
            queue_timeout_seconds: default_provider_queue_timeout_seconds(),
            max_retries: default_provider_max_retries(),
            retry_initial_backoff_ms: default_provider_retry_initial_backoff_ms(),
            retry_max_backoff_ms: default_provider_retry_max_backoff_ms(),
            retry_jitter: default_provider_retry_jitter(),
            connect_timeout_seconds: default_provider_connect_timeout_seconds(),
            first_token_timeout_seconds: default_provider_first_token_timeout_seconds(),
            stream_idle_timeout_seconds: default_provider_stream_idle_timeout_seconds(),
            total_generation_timeout_seconds: default_provider_total_generation_timeout_seconds(),
        }
    }
}

impl CustomProviderRequestPolicyConfig {
    fn validate(&self, provider_name: &str) -> Result<(), String> {
        if self.max_in_flight_requests == Some(0) {
            return Err(format!(
                "custom_providers[{provider_name}].request_policy: `max_in_flight_requests` must be greater than 0"
            ));
        }
        if self.queue_timeout_seconds == 0 {
            return Err(format!(
                "custom_providers[{provider_name}].request_policy: `queue_timeout_seconds` must be greater than 0"
            ));
        }
        if self.retry_initial_backoff_ms == 0 {
            return Err(format!(
                "custom_providers[{provider_name}].request_policy: `retry_initial_backoff_ms` must be greater than 0"
            ));
        }
        if self.retry_max_backoff_ms < self.retry_initial_backoff_ms {
            return Err(format!(
                "custom_providers[{provider_name}].request_policy: `retry_max_backoff_ms` must be greater than or equal to `retry_initial_backoff_ms`"
            ));
        }
        Ok(())
    }
}

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

    /// Optional default pricing in USD per million tokens.
    #[serde(default, skip_serializing_if = "CustomProviderPricingConfig::is_empty")]
    pub pricing: CustomProviderPricingConfig,

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_stream_usage: Option<bool>,

    /// Allow terminal Responses function-call IDs to replace mismatched
    /// streamed IDs after strict semantic one-to-one validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responses_allow_function_call_id_remap: Option<bool>,

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

    /// Provider response-header names carrying typed rate-limit metadata.
    #[serde(default, skip_serializing_if = "RateLimitHeaderConfig::is_default")]
    pub rate_limit_headers: RateLimitHeaderConfig,

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

    /// Resolve response header mappings, preserving every explicit mapping and
    /// filling provider-specific fields that were not configured.
    pub fn effective_rate_limit_headers(&self) -> RateLimitHeaderConfig {
        let mut headers = self.rate_limit_headers.clone();
        headers.fill_missing_from(&RateLimitHeaderConfig::for_provider_name(&self.name));
        headers
    }

    pub fn provider_defaults_profile(&self) -> CustomProviderProfileConfig {
        CustomProviderProfileConfig {
            api_format: self.api_format,
            context_window: self.context_window,
            pricing: self.pricing,
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
            responses_allow_function_call_id_remap: self.responses_allow_function_call_id_remap,
            supports_context_edits: self.supports_context_edits,
        }
    }

    /// Validate that required fields are present and the name doesn't collide
    /// with built-in provider keys.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("custom_providers: `name` must not be empty".to_string());
        }

        if !is_valid_provider_name(&self.name) {
            return Err(format!(
                "custom_providers[{}]: `name` must use lowercase letters, digits, hyphens, or underscores",
                self.name
            ));
        }

        if self.display_name.trim().is_empty() {
            return Err(format!("custom_providers[{}]: `display_name` must not be empty", self.name));
        }

        if self.base_url.trim().is_empty() {
            return Err(format!("custom_providers[{}]: `base_url` must not be empty", self.name));
        }

        if self.context_window == Some(0) {
            return Err(format!("custom_providers[{}]: `context_window` must be greater than 0", self.name));
        }

        self.pricing.validate(&format!("custom_providers[{}]", self.name))?;

        if let Some(temperature) = self.temperature
            && !(0.0..=2.0).contains(&temperature)
        {
            return Err(format!("custom_providers[{}]: `temperature` must be between 0.0 and 2.0", self.name));
        }

        if let Some(top_p) = self.top_p
            && !(0.0..=1.0).contains(&top_p)
        {
            return Err(format!("custom_providers[{}]: `top_p` must be between 0.0 and 1.0", self.name));
        }

        if let Some(top_k) = self.top_k
            && top_k < 0
        {
            return Err(format!("custom_providers[{}]: `top_k` must be >= 0", self.name));
        }

        for (field, value) in [
            ("`presence_penalty`", self.presence_penalty),
            ("`frequency_penalty`", self.frequency_penalty),
        ] {
            if let Some(value) = value
                && !(-2.0..=2.0).contains(&value)
            {
                return Err(format!("custom_providers[{}]: {field} must be between -2.0 and 2.0", self.name));
            }
        }

        if self.max_tokens == Some(0) {
            return Err(format!("custom_providers[{}]: `max_tokens` must be greater than 0", self.name));
        }

        if self.reasoning_effort == Some(ReasoningEffortLevel::Unknown) {
            return Err(format!(
                "custom_providers[{}]: `reasoning_effort` is not a recognized level (use none, minimal, low, medium, high, xhigh, or max)",
                self.name
            ));
        }

        if let Some(auth) = &self.auth {
            auth.validate(&self.name)?;
            if !self.api_key_env.trim().is_empty() {
                return Err(format!("custom_providers[{}]: `auth` cannot be combined with `api_key_env`", self.name));
            }
        }

        if !self.api_key_env.trim().is_empty()
            && let Err(err) = crate::auth::CredentialIdentity::new(&self.name, &self.api_key_env)
        {
            return Err(format!("custom_providers[{}]: invalid `api_key_env`: {err}", self.name));
        }

        if self.models.iter().any(|m| m.trim().is_empty()) {
            return Err(format!("custom_providers[{}]: `models` entries must not be empty", self.name));
        }

        self.request_policy.validate(&self.name)?;
        self.rate_limit_headers.validate(&self.name)?;

        for (profile_key, profile) in &self.profiles {
            if profile_key.trim().is_empty() || profile_key.trim() != profile_key {
                return Err(format!(
                    "custom_providers[{}]: profile key `{profile_key}` must not be empty or contain surrounding whitespace",
                    self.name
                ));
            }

            profile.validate(&self.name, profile_key)?;
        }

        let reserved = [
            "openai",
            "anthropic",
            "gemini",
            "copilot",
            "deepseek",
            "meta",
            "meta-ai",
            "openrouter",
            "ollama",
            "lmstudio",
            "llamacpp",
            "moonshot",
            "zai",
            "minimax",
            "huggingface",
            "openresponses",
        ];
        let lower = self.name.to_lowercase();
        if reserved.contains(&lower.as_str()) {
            return Err(format!("custom_providers[{}]: name collides with built-in provider", self.name));
        }

        Ok(())
    }
}

fn is_valid_provider_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    let Some(last) = bytes.last() else {
        return false;
    };

    let is_valid_char = |ch: u8| matches!(ch, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_');
    let is_alphanumeric = |ch: u8| matches!(ch, b'a'..=b'z' | b'0'..=b'9');

    is_alphanumeric(*first) && is_alphanumeric(*last) && bytes.iter().copied().all(is_valid_char)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::{
        CustomProviderApiFormat, CustomProviderCommandAuthConfig, CustomProviderConfig, CustomProviderPricingConfig,
        CustomProviderProfileConfig, CustomProviderRequestPolicyConfig, RateLimitHeaderConfig,
        ResolvedCustomProviderProfile, default_auth_refresh_interval_ms, default_auth_timeout_ms,
    };

    #[test]
    fn custom_provider_pricing_converts_per_million_rates_to_per_token_rates() {
        let pricing = CustomProviderPricingConfig {
            input_per_million_usd: Some(0.13),
            output_per_million_usd: Some(0.26),
            cache_read_per_million_usd: Some(0.028),
            cache_write_per_million_usd: None,
        }
        .model_pricing()
        .expect("complete custom pricing");

        assert_eq!(pricing.input, Some(0.13 / 1_000_000.0));
        assert_eq!(pricing.output, Some(0.26 / 1_000_000.0));
        assert_eq!(pricing.cache_read, Some(0.028 / 1_000_000.0));
        assert_eq!(pricing.cache_write, None);
    }

    #[test]
    fn custom_provider_pricing_rejects_negative_or_non_finite_rates() {
        for invalid_rate in [-0.01, f64::INFINITY, f64::NAN] {
            let pricing = CustomProviderPricingConfig {
                input_per_million_usd: Some(invalid_rate),
                ..Default::default()
            };
            assert!(pricing.validate("custom_providers[test]").is_err());
        }
    }

    #[test]
    fn validate_accepts_lowercase_provider_name() {
        let config = CustomProviderConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: None,
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            api_format: CustomProviderApiFormat::Auto,
            context_window: None,
            pricing: Default::default(),
            supports_tools: None,
            supports_reasoning: None,
            supports_reasoning_effort: None,
            supports_vision: None,
            supports_structured_output: None,
            supports_parallel_tool_calls: None,
            supports_context_caching: None,
            supports_responses_compaction: None,
            supports_stream_usage: None,
            responses_allow_function_call_id_remap: None,
            supports_context_edits: None,
            api_key_env: String::new(),
            auth: None,
            model: "gpt-5-mini".to_string(),
            models: Vec::new(),
            profiles: BTreeMap::new(),
            rate_limit_headers: RateLimitHeaderConfig::default(),
            request_policy: CustomProviderRequestPolicyConfig::default(),
        };

        assert!(config.validate().is_ok());
        assert_eq!(config.resolved_api_key_env(), "MYCORP_API_KEY");
    }

    #[test]
    fn validate_rejects_invalid_provider_name() {
        let config = CustomProviderConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: None,
            name: "My Corp".to_string(),
            display_name: "My Corp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            api_format: CustomProviderApiFormat::Auto,
            context_window: None,
            pricing: Default::default(),
            supports_tools: None,
            supports_reasoning: None,
            supports_reasoning_effort: None,
            supports_vision: None,
            supports_structured_output: None,
            supports_parallel_tool_calls: None,
            supports_context_caching: None,
            supports_responses_compaction: None,
            supports_stream_usage: None,
            responses_allow_function_call_id_remap: None,
            supports_context_edits: None,
            api_key_env: String::new(),
            auth: None,
            model: "gpt-5-mini".to_string(),
            models: Vec::new(),
            profiles: BTreeMap::new(),
            rate_limit_headers: RateLimitHeaderConfig::default(),
            request_policy: CustomProviderRequestPolicyConfig::default(),
        };

        let err = config.validate().expect_err("invalid name should fail");
        assert!(err.contains("must use lowercase letters, digits, hyphens, or underscores"));
    }

    #[test]
    fn validate_rejects_auth_and_api_key_env_together() {
        let config = CustomProviderConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: None,
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            api_format: CustomProviderApiFormat::Auto,
            context_window: None,
            pricing: Default::default(),
            supports_tools: None,
            supports_reasoning: None,
            supports_reasoning_effort: None,
            supports_vision: None,
            supports_structured_output: None,
            supports_parallel_tool_calls: None,
            supports_context_caching: None,
            supports_responses_compaction: None,
            supports_stream_usage: None,
            responses_allow_function_call_id_remap: None,
            supports_context_edits: None,
            api_key_env: "MYCORP_API_KEY".to_string(),
            auth: Some(CustomProviderCommandAuthConfig {
                command: "print-token".to_string(),
                args: Vec::new(),
                cwd: None,
                timeout_ms: default_auth_timeout_ms(),
                refresh_interval_ms: default_auth_refresh_interval_ms(),
            }),
            model: "gpt-5-mini".to_string(),
            models: Vec::new(),
            profiles: BTreeMap::new(),
            rate_limit_headers: RateLimitHeaderConfig::default(),
            request_policy: CustomProviderRequestPolicyConfig::default(),
        };

        let err = config.validate().expect_err("conflicting auth should fail");
        assert!(err.contains("`auth` cannot be combined with `api_key_env`"));
    }

    #[test]
    fn validate_accepts_command_auth_without_static_env_key() {
        let config = CustomProviderConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: None,
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            api_format: CustomProviderApiFormat::Auto,
            context_window: None,
            pricing: Default::default(),
            supports_tools: None,
            supports_reasoning: None,
            supports_reasoning_effort: None,
            supports_vision: None,
            supports_structured_output: None,
            supports_parallel_tool_calls: None,
            supports_context_caching: None,
            supports_responses_compaction: None,
            supports_stream_usage: None,
            responses_allow_function_call_id_remap: None,
            supports_context_edits: None,
            api_key_env: String::new(),
            auth: Some(CustomProviderCommandAuthConfig {
                command: "print-token".to_string(),
                args: vec!["--json".to_string()],
                cwd: Some(PathBuf::from("/tmp")),
                timeout_ms: 1_000,
                refresh_interval_ms: 60_000,
            }),
            model: "gpt-5-mini".to_string(),
            models: Vec::new(),
            profiles: BTreeMap::new(),
            rate_limit_headers: RateLimitHeaderConfig::default(),
            request_policy: CustomProviderRequestPolicyConfig::default(),
        };

        assert!(config.validate().is_ok());
        assert!(config.uses_command_auth());
    }

    #[test]
    fn validate_rejects_empty_model_entry_in_models_list() {
        let config = CustomProviderConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: None,
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            api_format: CustomProviderApiFormat::Auto,
            context_window: None,
            pricing: Default::default(),
            supports_tools: None,
            supports_reasoning: None,
            supports_reasoning_effort: None,
            supports_vision: None,
            supports_structured_output: None,
            supports_parallel_tool_calls: None,
            supports_context_caching: None,
            supports_responses_compaction: None,
            supports_stream_usage: None,
            responses_allow_function_call_id_remap: None,
            supports_context_edits: None,
            api_key_env: "MYCORP_API_KEY".to_string(),
            auth: None,
            model: "gpt-5-mini".to_string(),
            models: vec!["valid-model".to_string(), "   ".to_string()],
            profiles: BTreeMap::new(),
            rate_limit_headers: RateLimitHeaderConfig::default(),
            request_policy: CustomProviderRequestPolicyConfig::default(),
        };

        let err = config.validate().expect_err("blank models entry should fail");
        assert!(err.contains("`models` entries must not be empty"));
    }

    #[test]
    fn validate_rejects_zero_context_window() {
        let config = CustomProviderConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: None,
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            api_format: CustomProviderApiFormat::Auto,
            context_window: Some(0),
            pricing: Default::default(),
            supports_tools: None,
            supports_reasoning: None,
            supports_reasoning_effort: None,
            supports_vision: None,
            supports_structured_output: None,
            supports_parallel_tool_calls: None,
            supports_context_caching: None,
            supports_responses_compaction: None,
            supports_stream_usage: None,
            responses_allow_function_call_id_remap: None,
            supports_context_edits: None,
            api_key_env: String::new(),
            auth: None,
            model: "gpt-5-mini".to_string(),
            models: Vec::new(),
            profiles: BTreeMap::new(),
            rate_limit_headers: RateLimitHeaderConfig::default(),
            request_policy: CustomProviderRequestPolicyConfig::default(),
        };

        let err = config.validate().expect_err("zero context window should fail");
        assert!(err.contains("`context_window` must be greater than 0"));
    }

    #[test]
    fn validate_rejects_malformed_profile_key() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            " gpt-5-mini ".to_string(),
            CustomProviderProfileConfig {
                temperature: None,
                top_p: None,
                top_k: None,
                presence_penalty: None,
                frequency_penalty: None,
                max_tokens: None,
                reasoning_effort: None,
                api_format: CustomProviderApiFormat::Auto,
                context_window: Some(128_000),
                pricing: Default::default(),
                supports_tools: None,
                supports_reasoning: None,
                supports_reasoning_effort: None,
                supports_vision: None,
                supports_structured_output: None,
                supports_parallel_tool_calls: None,
                supports_context_caching: None,
                supports_responses_compaction: None,
                supports_stream_usage: None,
                responses_allow_function_call_id_remap: None,
                supports_context_edits: None,
            },
        );

        let config = CustomProviderConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: None,
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            api_format: CustomProviderApiFormat::Auto,
            context_window: None,
            pricing: Default::default(),
            supports_tools: None,
            supports_reasoning: None,
            supports_reasoning_effort: None,
            supports_vision: None,
            supports_structured_output: None,
            supports_parallel_tool_calls: None,
            supports_context_caching: None,
            supports_responses_compaction: None,
            supports_stream_usage: None,
            responses_allow_function_call_id_remap: None,
            supports_context_edits: None,
            api_key_env: String::new(),
            auth: None,
            model: "gpt-5-mini".to_string(),
            models: Vec::new(),
            profiles,
            rate_limit_headers: RateLimitHeaderConfig::default(),
            request_policy: CustomProviderRequestPolicyConfig::default(),
        };

        let err = config.validate().expect_err("profile key with whitespace should fail");
        assert!(err.contains("profile key"));
    }

    #[test]
    fn effective_models_uses_models_list_when_present() {
        let config = CustomProviderConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: None,
            name: "atlascloud".to_string(),
            display_name: "Atlas Cloud".to_string(),
            base_url: "https://api.atlascloud.ai/v1".to_string(),
            api_format: CustomProviderApiFormat::Auto,
            context_window: None,
            pricing: Default::default(),
            supports_tools: None,
            supports_reasoning: None,
            supports_reasoning_effort: None,
            supports_vision: None,
            supports_structured_output: None,
            supports_parallel_tool_calls: None,
            supports_context_caching: None,
            supports_responses_compaction: None,
            supports_stream_usage: None,
            responses_allow_function_call_id_remap: None,
            supports_context_edits: None,
            api_key_env: "ATLASCLOUD_API_KEY".to_string(),
            auth: None,
            model: "deepseek-ai/deepseek-v4-flash".to_string(),
            models: vec![
                "deepseek-ai/deepseek-v4-flash".to_string(),
                "deepseek-ai/deepseek-v4-pro".to_string(),
                "deepseek-ai/DeepSeek-V3-0324".to_string(),
                "qwen/qwen3.6-35b-a3b".to_string(),
                "moonshotai/kimi-k2.7-code".to_string(),
                "moonshotai/kimi-k2.6".to_string(),
                "zai-org/glm-5.2".to_string(),
                "minimaxai/minimax-m3".to_string(),
            ],
            profiles: BTreeMap::new(),
            rate_limit_headers: RateLimitHeaderConfig::default(),
            request_policy: CustomProviderRequestPolicyConfig::default(),
        };

        assert_eq!(
            config.effective_models(),
            vec![
                "deepseek-ai/deepseek-v4-flash".to_string(),
                "deepseek-ai/deepseek-v4-pro".to_string(),
                "deepseek-ai/DeepSeek-V3-0324".to_string(),
                "qwen/qwen3.6-35b-a3b".to_string(),
                "moonshotai/kimi-k2.7-code".to_string(),
                "moonshotai/kimi-k2.6".to_string(),
                "zai-org/glm-5.2".to_string(),
                "minimaxai/minimax-m3".to_string(),
            ]
        );
    }

    #[test]
    fn effective_models_falls_back_to_single_model_field() {
        let config = CustomProviderConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: None,
            model: "gpt-5-mini".to_string(),
            ..CustomProviderConfig::default()
        };

        assert_eq!(config.effective_models(), vec!["gpt-5-mini".to_string()]);
    }

    #[test]
    fn resolved_profile_prefers_exact_model_key() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "gpt-5-mini".to_string(),
            CustomProviderProfileConfig {
                temperature: None,
                top_p: None,
                top_k: None,
                presence_penalty: None,
                frequency_penalty: None,
                max_tokens: None,
                reasoning_effort: None,
                api_format: CustomProviderApiFormat::OpenAIResponses,
                context_window: Some(128_000),
                pricing: Default::default(),
                supports_tools: Some(true),
                supports_reasoning: None,
                supports_reasoning_effort: None,
                supports_vision: None,
                supports_structured_output: None,
                supports_parallel_tool_calls: None,
                supports_context_caching: None,
                supports_responses_compaction: None,
                supports_stream_usage: None,
                responses_allow_function_call_id_remap: None,
                supports_context_edits: None,
            },
        );

        let config = CustomProviderConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: None,
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            api_format: CustomProviderApiFormat::OpenAIChat,
            context_window: Some(256_000),
            pricing: Default::default(),
            supports_tools: Some(true),
            supports_reasoning: Some(true),
            supports_reasoning_effort: None,
            supports_vision: None,
            supports_structured_output: None,
            supports_parallel_tool_calls: None,
            supports_context_caching: None,
            supports_responses_compaction: None,
            supports_stream_usage: None,
            responses_allow_function_call_id_remap: None,
            supports_context_edits: None,
            api_key_env: String::new(),
            auth: None,
            model: "gpt-5-mini".to_string(),
            models: Vec::new(),
            profiles,
            rate_limit_headers: RateLimitHeaderConfig::default(),
            request_policy: CustomProviderRequestPolicyConfig::default(),
        };

        let resolved = config.resolved_profile("gpt-5-mini");
        assert_eq!(
            resolved,
            ResolvedCustomProviderProfile {
                api_format: Some(CustomProviderApiFormat::OpenAIResponses),
                context_window: Some(128_000),
                pricing: Default::default(),
                temperature: None,
                top_p: None,
                top_k: None,
                presence_penalty: None,
                frequency_penalty: None,
                max_tokens: None,
                reasoning_effort: None,
                supports_tools: Some(true),
                supports_reasoning: Some(true),
                supports_reasoning_effort: None,
                supports_vision: None,
                supports_structured_output: None,
                supports_parallel_tool_calls: None,
                supports_context_caching: None,
                supports_responses_compaction: None,
                supports_stream_usage: None,
                responses_allow_function_call_id_remap: None,
                supports_context_edits: None,
            }
        );
        assert!(config.profile("gpt-5").is_none());
    }

    #[test]
    fn sparse_inheritance_preserves_provider_defaults() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "gpt-5-mini".to_string(),
            CustomProviderProfileConfig {
                temperature: None,
                top_p: None,
                top_k: None,
                presence_penalty: None,
                frequency_penalty: None,
                max_tokens: None,
                reasoning_effort: None,
                api_format: CustomProviderApiFormat::Auto,
                context_window: None,
                pricing: Default::default(),
                supports_tools: Some(false),
                supports_reasoning: None,
                supports_reasoning_effort: Some(true),
                supports_vision: None,
                supports_structured_output: None,
                supports_parallel_tool_calls: None,
                supports_context_caching: None,
                supports_responses_compaction: None,
                supports_stream_usage: Some(false),
                responses_allow_function_call_id_remap: None,
                supports_context_edits: None,
            },
        );

        let config = CustomProviderConfig {
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: None,
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            api_format: CustomProviderApiFormat::OpenAIChat,
            context_window: Some(256_000),
            pricing: Default::default(),
            supports_tools: Some(true),
            supports_reasoning: Some(false),
            supports_reasoning_effort: None,
            supports_vision: Some(true),
            supports_structured_output: None,
            supports_parallel_tool_calls: Some(true),
            supports_context_caching: Some(false),
            supports_responses_compaction: None,
            supports_stream_usage: Some(true),
            responses_allow_function_call_id_remap: None,
            supports_context_edits: None,
            api_key_env: String::new(),
            auth: None,
            model: "gpt-5-mini".to_string(),
            models: Vec::new(),
            profiles,
            rate_limit_headers: RateLimitHeaderConfig::default(),
            request_policy: CustomProviderRequestPolicyConfig::default(),
        };

        let resolved = config.resolved_profile("gpt-5-mini");
        assert_eq!(resolved.api_format, Some(CustomProviderApiFormat::OpenAIChat));
        assert_eq!(resolved.context_window, Some(256_000));
        assert_eq!(resolved.supports_tools, Some(false));
        assert_eq!(resolved.supports_reasoning, Some(false));
        assert_eq!(resolved.supports_reasoning_effort, Some(true));
        assert_eq!(resolved.supports_vision, Some(true));
        assert_eq!(resolved.supports_parallel_tool_calls, Some(true));
        assert_eq!(resolved.supports_context_caching, Some(false));
        assert_eq!(resolved.supports_responses_compaction, None);
        assert_eq!(resolved.supports_stream_usage, Some(false));
        assert_eq!(resolved.supports_context_edits, None);
    }

    #[test]
    fn legacy_default_behavior_retains_auto_and_empty_profiles() {
        let parsed: CustomProviderConfig = toml::from_str(
            r#"
name = "mycorp"
display_name = "MyCorp"
base_url = "https://llm.example/v1"
model = "gpt-5-mini"
"#,
        )
        .expect("legacy custom provider config should parse");

        assert_eq!(parsed.api_format, CustomProviderApiFormat::Auto);
        assert!(parsed.profiles.is_empty());
        assert_eq!(parsed.resolved_profile("gpt-5-mini"), ResolvedCustomProviderProfile::default());
        assert_eq!(parsed.rate_limit_headers, RateLimitHeaderConfig::default());
        assert_eq!(parsed.request_policy, CustomProviderRequestPolicyConfig::default());
    }

    #[test]
    fn responses_function_call_id_remap_is_explicit_and_profile_false_overrides_provider_true() {
        let parsed: CustomProviderConfig = toml::from_str(
            r#"
name = "friendli"
display_name = "Friendli"
base_url = "https://api.friendli.ai/serverless/v1"
model = "friendli-model"
responses_allow_function_call_id_remap = true

[profiles.friendli-model]
responses_allow_function_call_id_remap = false
"#,
        )
        .expect("function-call ID remap capability should parse");

        let encoded = serde_json::to_value(&parsed).expect("custom provider config should serialize");
        assert_eq!(
            encoded.get("responses_allow_function_call_id_remap"),
            Some(&serde_json::Value::Bool(true)),
            "the provider opt-in must not be dropped"
        );
        assert_eq!(
            encoded
                .get("profiles")
                .and_then(|profiles| profiles.get("friendli-model"))
                .and_then(|profile| profile.get("responses_allow_function_call_id_remap")),
            Some(&serde_json::Value::Bool(false)),
            "an explicit model-profile false must survive and override the provider opt-in"
        );
        assert_eq!(parsed.resolved_profile("friendli-model").responses_allow_function_call_id_remap, Some(false));
        assert_eq!(
            parsed
                .resolved_profile("unprofiled-model")
                .responses_allow_function_call_id_remap,
            Some(true)
        );
    }

    #[test]
    fn responses_function_call_id_remap_defaults_to_absent_and_disabled() {
        let parsed: CustomProviderConfig = toml::from_str(
            r#"
name = "strict"
display_name = "Strict"
base_url = "https://llm.example/v1"
model = "strict-model"
"#,
        )
        .expect("legacy custom provider config should parse");

        let encoded = serde_json::to_value(&parsed).expect("custom provider config should serialize");
        assert!(
            encoded.get("responses_allow_function_call_id_remap").is_none(),
            "the compatibility capability must remain an explicit opt-in"
        );
        assert_eq!(parsed.resolved_profile("strict-model").responses_allow_function_call_id_remap, None);
    }

    #[test]
    fn baseten_style_headers_are_the_default_for_custom_provider_aliases() {
        let config = CustomProviderConfig {
            name: "baseten-glm".to_string(),
            ..CustomProviderConfig::default()
        };

        let headers = config.effective_rate_limit_headers();
        assert_eq!(headers.requests_limit_per_minute.as_deref(), Some("x-ratelimit-limit-requests"));
        assert_eq!(headers.requests_remaining_per_minute.as_deref(), Some("x-ratelimit-remaining-requests"));
        assert_eq!(headers.tokens_limit_per_minute.as_deref(), Some("x-ratelimit-limit-tokens"));
        assert_eq!(headers.tokens_remaining_per_minute.as_deref(), Some("x-ratelimit-remaining-tokens"));
    }

    #[test]
    fn fireworks_defaults_keep_limits_and_request_counters_distinct() {
        let config = CustomProviderConfig {
            name: "fireworks-private".to_string(),
            ..CustomProviderConfig::default()
        };

        let headers = config.effective_rate_limit_headers();
        assert_eq!(headers.prompt_tokens_limit_per_second.as_deref(), Some("x-ratelimit-limit-tokens-prompt"));
        assert_eq!(
            headers.cache_adjusted_prompt_tokens_limit_per_second.as_deref(),
            Some("x-ratelimit-limit-tokens-cache-adjusted-prompt")
        );
        assert_eq!(headers.generated_tokens_limit_per_second.as_deref(), Some("x-ratelimit-limit-tokens-generated"));
        assert_eq!(headers.prompt_tokens.as_deref(), Some("fireworks-prompt-tokens"));
        assert_eq!(headers.cached_prompt_tokens.as_deref(), Some("fireworks-cached-prompt-tokens"));
    }

    #[test]
    fn together_defaults_preserve_per_second_units_and_reset_header() {
        let config = CustomProviderConfig {
            name: "together-router".to_string(),
            ..CustomProviderConfig::default()
        };

        let headers = config.effective_rate_limit_headers();
        assert_eq!(headers.requests_limit_per_second.as_deref(), Some("x-ratelimit-limit"));
        assert_eq!(headers.tokens_limit_per_second.as_deref(), Some("x-tokenlimit-limit"));
        assert_eq!(headers.reset_after_seconds.as_deref(), Some("x-ratelimit-reset"));
    }

    #[test]
    fn explicit_header_mapping_wins_over_provider_defaults() {
        let config = CustomProviderConfig {
            name: "together-proxy".to_string(),
            rate_limit_headers: RateLimitHeaderConfig {
                tokens_limit_per_second: Some("x-proxy-token-limit".to_string()),
                ..RateLimitHeaderConfig::default()
            },
            ..CustomProviderConfig::default()
        };

        let headers = config.effective_rate_limit_headers();
        assert_eq!(headers.tokens_limit_per_second.as_deref(), Some("x-proxy-token-limit"));
        assert_eq!(headers.reset_after_seconds.as_deref(), Some("x-ratelimit-reset"));
    }

    #[test]
    fn validation_rejects_invalid_rate_limit_header_names() {
        let config = CustomProviderConfig {
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            rate_limit_headers: RateLimitHeaderConfig {
                prompt_tokens: Some("not a header".to_string()),
                ..RateLimitHeaderConfig::default()
            },
            ..CustomProviderConfig::default()
        };

        assert!(config.validate().is_err_and(|error| error.contains("prompt_tokens")));
    }

    #[test]
    fn request_policy_defaults_allow_five_exponential_retry_delays() {
        let policy = CustomProviderRequestPolicyConfig::default();

        assert_eq!(policy.retry_initial_backoff_ms, 10_000);
        assert_eq!(policy.retry_max_backoff_ms, 160_000);
    }

    #[test]
    fn request_policy_deserializes_provider_limits_and_retries() {
        let parsed: CustomProviderConfig = toml::from_str(
            r#"
name = "mycorp"
display_name = "MyCorp"
base_url = "https://llm.example/v1"
model = "gpt-5-mini"

[request_policy]
max_in_flight_requests = 3
queue_timeout_seconds = 120
max_retries = 4
retry_initial_backoff_ms = 250
retry_max_backoff_ms = 5000
retry_jitter = false
connect_timeout_seconds = 45
first_token_timeout_seconds = 240
stream_idle_timeout_seconds = 150
total_generation_timeout_seconds = 900
"#,
        )
        .expect("custom provider request policy should parse");

        assert_eq!(parsed.request_policy.max_in_flight_requests, Some(3));
        assert_eq!(parsed.request_policy.queue_timeout_seconds, 120);
        assert_eq!(parsed.request_policy.max_retries, 4);
        assert_eq!(parsed.request_policy.retry_initial_backoff_ms, 250);
        assert_eq!(parsed.request_policy.retry_max_backoff_ms, 5_000);
        assert!(!parsed.request_policy.retry_jitter);
        assert_eq!(parsed.request_policy.connect_timeout_seconds, 45);
        assert_eq!(parsed.request_policy.first_token_timeout_seconds, 240);
        assert_eq!(parsed.request_policy.stream_idle_timeout_seconds, 150);
        assert_eq!(parsed.request_policy.total_generation_timeout_seconds, 900);
        assert!(parsed.validate().is_ok());
    }

    #[test]
    fn request_policy_validation_rejects_unsafe_bounds() {
        let mut config = CustomProviderConfig {
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            model: "gpt-5-mini".to_string(),
            ..CustomProviderConfig::default()
        };

        config.request_policy.max_in_flight_requests = Some(0);
        assert!(config.validate().is_err_and(|error| error.contains("max_in_flight_requests")));

        config.request_policy.max_in_flight_requests = Some(1);
        config.request_policy.retry_initial_backoff_ms = 2_000;
        config.request_policy.retry_max_backoff_ms = 1_000;
        assert!(config.validate().is_err_and(|error| error.contains("retry_max_backoff_ms")));
    }

    #[test]
    fn deserialize_rejects_invalid_api_format() {
        let err = toml::from_str::<CustomProviderConfig>(
            r#"
name = "mycorp"
display_name = "MyCorp"
base_url = "https://llm.example/v1"
api_format = "openai-chatty"
"#,
        )
        .expect_err("invalid api_format should fail");

        assert!(err.to_string().contains("openai-chatty"));
    }
}

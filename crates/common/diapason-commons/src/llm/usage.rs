//! Provider-independent LLM usage and balance types.

use serde::{Deserialize, Serialize};

/// Identifies the backend family that produced an LLM response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendKind {
    /// Google Gemini.
    Gemini,
    /// OpenAI.
    OpenAI,
    /// Anthropic.
    Anthropic,
    /// DeepSeek.
    DeepSeek,
    /// Meta.
    Meta,
    /// Mistral.
    Mistral,
    /// OpenRouter.
    OpenRouter,
    /// Ollama (local).
    Ollama,
    /// llama.cpp (local).
    LlamaCpp,
    /// Z.AI.
    ZAI,
    /// Moonshot.
    Moonshot,
    /// Hugging Face.
    HuggingFace,
    /// MiniMax.
    Minimax,
    /// MiMo.
    MiMo,
    /// OpenCode Zen.
    OpenCodeZen,
    /// OpenCode Go.
    OpenCodeGo,
    /// Qwen.
    Qwen,
    /// StepFun.
    StepFun,
    /// Evolink.
    Evolink,
    /// Poolside.
    Poolside,
    /// xAI.
    Xai,
    /// NVIDIA.
    Nvidia,
    /// Merge Gateway.
    MergeGateway,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
/// Token counts reported for one LLM response.
pub struct Usage {
    /// Tokens consumed by the prompt.
    pub prompt_tokens: u32,
    /// Tokens produced in the completion.
    pub completion_tokens: u32,
    /// Total tokens billed for the response.
    pub total_tokens: u32,
    /// Prompt tokens served from cache, when reported.
    pub cached_prompt_tokens: Option<u32>,
    /// Tokens written to the prompt cache, when reported.
    pub cache_creation_tokens: Option<u32>,
    /// Tokens read from the prompt cache, when reported.
    pub cache_read_tokens: Option<u32>,
    /// Per-iteration token usage for Anthropic server-side fallback and compaction.
    /// Each entry represents one sampling pass (message, fallback_message, or compaction).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iterations: Option<Vec<serde_json::Value>>,
}

impl Usage {
    #[inline]
    fn has_cache_read_metric(&self) -> bool {
        self.cache_read_tokens.is_some() || self.cached_prompt_tokens.is_some()
    }

    #[inline]
    fn has_any_cache_metrics(&self) -> bool {
        self.has_cache_read_metric() || self.cache_creation_tokens.is_some()
    }

    /// Returns cache reads, preferring the current metric over the legacy prompt count.
    #[inline]
    pub fn cache_read_tokens_or_fallback(&self) -> u32 {
        self.cache_read_tokens.or(self.cached_prompt_tokens).unwrap_or(0)
    }

    /// Returns cache-creation tokens, or zero when the provider omitted the metric.
    #[inline]
    pub fn cache_creation_tokens_or_zero(&self) -> u32 {
        self.cache_creation_tokens.unwrap_or(0)
    }

    /// Returns cache reads as a percentage of all reported cache activity.
    #[inline]
    pub fn cache_hit_rate(&self) -> Option<f64> {
        if !self.has_any_cache_metrics() {
            return None;
        }
        let read = f64::from(self.cache_read_tokens_or_fallback());
        let creation = f64::from(self.cache_creation_tokens_or_zero());
        let total = read + creation;
        if total > 0.0 {
            Some((read / total) * 100.0)
        } else {
            None
        }
    }

    #[inline]
    #[cfg(test)]
    fn is_cache_hit(&self) -> Option<bool> {
        self.has_any_cache_metrics().then(|| self.cache_read_tokens_or_fallback() > 0)
    }

    #[inline]
    #[cfg(test)]
    fn is_cache_miss(&self) -> Option<bool> {
        self.has_any_cache_metrics()
            .then(|| self.cache_creation_tokens_or_zero() > 0 && self.cache_read_tokens_or_fallback() == 0)
    }

    #[inline]
    #[cfg(test)]
    fn total_cache_tokens(&self) -> u32 {
        let read = self.cache_read_tokens_or_fallback();
        let creation = self.cache_creation_tokens_or_zero();
        read + creation
    }

    #[inline]
    #[cfg(test)]
    fn cache_savings_ratio(&self) -> Option<f64> {
        if !self.has_cache_read_metric() {
            return None;
        }
        let read = f64::from(self.cache_read_tokens_or_fallback());
        let prompt = f64::from(self.prompt_tokens);
        if prompt > 0.0 { Some(read / prompt) } else { None }
    }
}

/// Provider-agnostic balance information for account status display.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BalanceInfo {
    /// Human-readable balance string (e.g. "100.00¥", "$50.00").
    pub display: String,
    /// Whether the account has sufficient balance for API calls.
    pub is_available: bool,
}

/// DeepSeek-specific balance info from GET /user/balance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepSeekBalanceResponse {
    is_available: bool,
    balance_infos: Vec<DeepSeekCurrencyBalance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Balance reported for one currency.
pub struct DeepSeekCurrencyBalance {
    currency: String,
    total_balance: String,
    #[serde(default)]
    granted_balance: String,
    #[serde(default)]
    topped_up_balance: String,
}

impl From<DeepSeekBalanceResponse> for BalanceInfo {
    fn from(resp: DeepSeekBalanceResponse) -> Self {
        let display = resp
            .balance_infos
            .first()
            .map(|b| {
                let symbol = match b.currency.as_str() {
                    "CNY" => "¥",
                    "USD" => "$",
                    _ => &b.currency,
                };
                format!("{}{}", b.total_balance, symbol)
            })
            .unwrap_or_else(|| "N/A".to_string());
        BalanceInfo { display, is_available: resp.is_available }
    }
}
/// Explains why an LLM response stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FinishReason {
    #[default]
    /// The model stopped naturally.
    Stop,
    /// The output hit the token limit.
    Length,
    /// The model requested tool calls.
    ToolCalls,
    /// Content was withheld by a filter.
    ContentFilter,
    /// Generation paused and can be resumed.
    Pause,
    /// The model refused to continue.
    Refusal,
    /// The provider reported an error with the given message.
    Error(String),
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;

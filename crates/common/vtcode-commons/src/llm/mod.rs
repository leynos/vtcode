//! Core LLM types shared across VT Code.
//!
//! This module keeps provider-independent wire types, usage accounting, tool
//! calls, responses, and sanitized error metadata at a stable public path.

mod errors;
mod tool_calls;
mod usage;

pub use errors::{LLMError, LLMErrorMetadata};
pub use tool_calls::{FunctionCall, ToolCall};
pub use usage::{BackendKind, BalanceInfo, DeepSeekBalanceResponse, DeepSeekCurrencyBalance, FinishReason, Usage};

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

/// Universal LLM response structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LLMResponse {
    /// The response content text
    pub content: Option<String>,

    /// Tool calls made by the model
    pub tool_calls: Option<Vec<ToolCall>>,

    /// The model that generated this response
    pub model: String,

    /// Token usage statistics
    pub usage: Option<Usage>,

    /// Why the response finished
    pub finish_reason: FinishReason,

    /// Reasoning content (for models that support it)
    pub reasoning: Option<String>,

    /// Detailed reasoning traces (for models that support it)
    pub reasoning_details: Option<Vec<String>>,

    /// Tool references for context
    pub tool_references: Vec<String>,

    /// Request ID from the provider
    pub request_id: Option<String>,

    /// Organization ID from the provider
    pub organization_id: Option<String>,

    /// Compaction summary content from Anthropic's server-side compaction.
    /// Populated when `stop_reason` is `Pause` (from `"compaction"`).
    /// The caller should pass this back in subsequent requests so the API
    /// can drop prior messages before the compaction block.
    pub compaction: Option<String>,
}

impl LLMResponse {
    /// Creates an LLM response with mandatory fields.
    pub fn new(model: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            content: Some(content.into()),
            tool_calls: None,
            model: model.into(),
            usage: None,
            finish_reason: FinishReason::Stop,
            reasoning: None,
            reasoning_details: None,
            tool_references: Vec::new(),
            request_id: None,
            organization_id: None,
            compaction: None,
        }
    }

    /// Returns response content or an empty string.
    pub fn content_text(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }

    /// Returns a cloned response content string.
    pub fn content_string(&self) -> String {
        self.content.clone().unwrap_or_default()
    }
}

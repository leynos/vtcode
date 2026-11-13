//! LLM provider implementations.
//!
//! All provider implementations live in [`diapason_llm::providers`]. This module
//! re-exports them so the historical `crate::llm::providers::*` import paths
//! continue to resolve throughout `diapason-core`.

pub use diapason_llm::providers::reasoning::{
    ReasoningBuffer, ReasoningSegment, extract_reasoning_trace, split_reasoning_from_text,
};
pub use diapason_llm::providers::{
    AnthropicProvider, CopilotProvider, DeepSeekProvider, EvolinkProvider, GeminiProvider, HuggingFaceProvider,
    LlamaCppProvider, LmStudioProvider, MergeCatalogueAvailability, MergeCatalogueFilters, MergeCatalogueModel,
    MergeCatalogueServiceTier, MergeCatalogueSnapshot, MergeGatewayCatalogueClient, MergeGatewayProvider, MetaProvider,
    MiMoProvider, MinimaxProvider, MistralProvider, MoonshotProvider, NvidiaProvider, OllamaProvider, OpenAIProvider,
    OpenCodeGoProvider, OpenCodeZenProvider, OpenResponsesProvider, OpenRouterProvider, PoolsideProvider, QwenProvider,
    StepFunProvider, StreamingProgressBuilder, StreamingProgressCallback, StreamingProgressTracker, TagStreamSanitizer,
    XAIProvider, ZAIProvider, clean_reasoning_text, reasoning,
};

// Re-export commonly used constants from diapason-commons::tool_types
pub use diapason_commons::tool_types::{
    DEFAULT_VEC_CAPACITY, ERROR_DETECTION_PATTERNS, MAX_SEARCH_RESULTS, OVERFLOW_INDICATOR_PREFIX,
};

// Submodule re-exports for code that accesses provider internals (e.g. gemini wire,
// anthropic request_builder, shared stream utilities).
pub use diapason_llm::providers::anthropic;
pub use diapason_llm::providers::anthropic_types;
pub use diapason_llm::providers::base;
pub use diapason_llm::providers::common;
pub use diapason_llm::providers::copilot;
pub use diapason_llm::providers::error_handling;
pub use diapason_llm::providers::evolink;
pub use diapason_llm::providers::gemini;
pub use diapason_llm::providers::huggingface;
pub use diapason_llm::providers::llamacpp;
pub use diapason_llm::providers::lmstudio;
pub use diapason_llm::providers::local_server;
pub use diapason_llm::providers::merge_gateway;
pub use diapason_llm::providers::merge_gateway_catalogue;
pub use diapason_llm::providers::meta;
pub use diapason_llm::providers::mimo;
pub use diapason_llm::providers::minimax;
pub use diapason_llm::providers::mistral;
pub use diapason_llm::providers::moonshot;
pub use diapason_llm::providers::nvidia;
pub use diapason_llm::providers::ollama;
pub use diapason_llm::providers::opencode_go;
pub use diapason_llm::providers::opencode_zen;
pub use diapason_llm::providers::openresponses;
pub use diapason_llm::providers::openrouter;
pub use diapason_llm::providers::poolside;
pub use diapason_llm::providers::qwen;
pub use diapason_llm::providers::shared;
pub use diapason_llm::providers::stepfun;
pub use diapason_llm::providers::streaming_progress;
pub use diapason_llm::providers::zai;

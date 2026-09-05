use std::collections::BTreeMap;

use serde::Serialize;

/// Aggregate token and prompt-cache usage found in a trace.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct TokenUsage {
    /// Total prompt/input tokens.
    pub input_tokens: u64,
    /// Total generated/output tokens.
    pub output_tokens: u64,
    /// Total prompt tokens served from cache.
    pub cached_input_tokens: u64,
    /// Total tokens used to create cache entries.
    pub cache_creation_tokens: u64,
    /// Total generated reasoning tokens when the provider reports them.
    pub reasoning_tokens: u64,
}

/// Statistics over recorded latency samples, in milliseconds.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq)]
pub struct LatencyStatistics {
    /// Number of latency samples.
    pub count: u64,
    /// Sum of latency samples.
    pub total_ms: u64,
    /// Arithmetic mean, or `None` when no samples were recorded.
    pub mean_ms: Option<f64>,
    /// Median from the bounded latency reservoir, or `None` when no samples were recorded.
    pub p50_ms: Option<u64>,
    /// 95th percentile from the bounded latency reservoir, or `None` when no samples were recorded.
    pub p95_ms: Option<u64>,
    /// Largest recorded sample, or `None` when no samples were recorded.
    pub max_ms: Option<u64>,
}

/// Redacted aggregate facts extracted from DeepSeek or VT Code JSONL traces.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct HarnessTraceSummary {
    /// Number of execution turns.
    pub turns: u64,
    /// Number of agent steps.
    pub steps: u64,
    /// Number of tool calls.
    pub tool_calls: u64,
    /// Tool name to invocation count.
    pub tool_counts: BTreeMap<String, u64>,
    /// Canonical error category to count.
    pub error_categories: BTreeMap<String, u64>,
    /// Latency aggregate for all recognized samples.
    pub latency: LatencyStatistics,
    /// Total UTF-8 byte length of tool outputs, without retaining output text.
    pub output_bytes: u64,
    /// Number of calls after the first call for each tool name.
    pub repeated_calls: u64,
    /// Repeated calls grouped by tool name.
    pub repeated_tool_counts: BTreeMap<String, u64>,
    /// Aggregate model token usage.
    pub token_usage: TokenUsage,
    /// Lines that were not valid JSON objects.
    pub malformed_lines: u64,
    /// Valid JSON objects with no recognized trace shape.
    pub unrecognized_lines: u64,
}

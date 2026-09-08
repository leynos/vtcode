//! Classification of erased and message-based errors.

use std::borrow::Cow;

use super::ErrorCategory;

#[must_use]
pub fn classify_anyhow_error(err: &anyhow::Error) -> ErrorCategory {
    let msg = err.to_string().to_ascii_lowercase();
    classify_error_message(&msg)
}

/// Return whether an error explicitly reports that the provider rejected the
/// request because its input context is too large.
///
/// Context-capacity failures are intentionally kept separate from
/// [`ErrorCategory::InvalidParameters`]: they are not model argument mistakes
/// and a bounded history compaction can make the same request valid. The
/// marker set is deliberately specific so ordinary validation messages that
/// merely mention a context field do not enter recovery.
#[must_use]
pub fn is_context_capacity_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| is_context_capacity_message(&cause.to_string()))
}

/// Return whether a provider error message identifies an input-context limit.
#[must_use]
pub fn is_context_capacity_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    contains_any(
        &message,
        &[
            "context_length_exceeded",
            "context length exceeded",
            "maximum context length",
            "maximum context window",
            "context window exceeded",
            "context window is too small",
            "exceeds the model's maximum context",
            "exceeds model context",
            "exceeds the maximum context",
            "prompt is too long",
            "input is too long",
            "input token count exceeds",
            "exceeds the maximum number of tokens",
            "maximum input tokens",
            "too many tokens in the prompt",
            "request exceeds the context",
        ],
    )
}

/// Classify an error message string into an `ErrorCategory`.
///
/// Marker groups are checked in priority order to handle overlapping patterns
/// (e.g., "tool permission denied by policy" → `PolicyViolation`, not `PermissionDenied`).
#[inline]
#[must_use]
pub fn classify_error_message(msg: &str) -> ErrorCategory {
    let msg = if msg.as_bytes().iter().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(msg.to_ascii_lowercase())
    } else {
        Cow::Borrowed(msg)
    };

    // --- Priority 1: Policy violations (before permission checks) ---
    if contains_any(
        &msg,
        &[
            "policy violation",
            "denied by policy",
            "tool permission denied",
            "safety validation failed",
            "not allowed in planning workflow",
            "only available when planning workflow is active",
            "workspace boundary",
            "blocked by policy",
        ],
    ) {
        return ErrorCategory::PolicyViolation;
    }

    // --- Priority 2: Planning workflow violations ---
    if contains_any(
        &msg,
        &[
            "planning workflow",
            "read-only permissions",
            concat!("read-only ", "mode"),
            "planning_policy_violation",
        ],
    ) {
        return ErrorCategory::PlanningPolicyViolation;
    }

    // --- Priority 3: Authentication / Authorization ---
    if contains_any(
        &msg,
        &[
            "invalid api key",
            "authentication failed",
            "unauthorized",
            "401",
            "invalid credentials",
        ],
    ) {
        return ErrorCategory::Authentication;
    }

    // --- Priority 4: Non-retryable resource exhaustion (billing, quotas) ---
    if contains_any(
        &msg,
        &[
            "weekly usage limit",
            "daily usage limit",
            "monthly spending limit",
            "insufficient credits",
            "quota exceeded",
            "billing",
            "payment required",
        ],
    ) {
        return ErrorCategory::ResourceExhausted;
    }

    // --- Priority 5: Invalid parameters ---
    if contains_any(
        &msg,
        &[
            "invalid argument",
            "invalid parameters",
            "invalid type",
            "malformed",
            "failed to parse arguments",
            "failed to parse argument",
            "missing required",
            "at least one item is required",
            "is required for",
            "schema validation",
            "argument validation failed",
            "unknown field",
            "unknown variant",
            "expected struct",
            "expected enum",
            "type mismatch",
            "must be an absolute path",
            "not parseable",
            "parseable as",
            // Patch format errors — these are LLM argument mistakes (the model
            // sent a malformed patch), not execution failures. Classifying them
            // as InvalidParameters ensures they don't trip the circuit breaker
            // (is_llm_mistake() == true) and get parameter-focused recovery
            // suggestions. See checkpoint turn_615 for the failure this
            // prevents: a unified-diff patch was classified as ExecutionError
            // and got generic "check tool documentation" suggestions.
            "invalid patch format",
            "invalid patch hunk",
            "invalid patch operation",
            "cannot parse empty patch",
            "patch does not contain",
            "semantic patch anchor",
        ],
    ) {
        return ErrorCategory::InvalidParameters;
    }

    // --- Priority 6: Tool not found ---
    if contains_any(&msg, &["tool not found", "unknown tool", "unsupported tool", "no such tool"]) {
        return ErrorCategory::ToolNotFound;
    }

    // --- Priority 7: Resource not found ---
    if contains_any(
        &msg,
        &[
            "no such file",
            "no such directory",
            "file not found",
            "directory not found",
            "resource not found",
            "path not found",
            "does not exist",
            "enoent",
        ],
    ) {
        return ErrorCategory::ResourceNotFound;
    }

    // --- Priority 8: Permission denied (OS-level) ---
    if contains_any(
        &msg,
        &[
            "permission denied",
            "access denied",
            "operation not permitted",
            "eacces",
            "eperm",
            "forbidden",
            "403",
        ],
    ) {
        return ErrorCategory::PermissionDenied;
    }

    // --- Priority 9: Cancellation ---
    if contains_any(&msg, &["cancelled", "interrupted", "canceled"]) {
        return ErrorCategory::Cancelled;
    }

    // --- Priority 10: Circuit breaker ---
    if contains_any(&msg, &["circuit breaker", "circuit open"]) {
        return ErrorCategory::CircuitOpen;
    }

    // --- Priority 11: Sandbox ---
    if contains_any(&msg, &["sandbox denied", "sandbox failure"]) {
        return ErrorCategory::SandboxFailure;
    }

    // --- Priority 12: Rate limiting (before general network) ---
    if contains_any(&msg, &["rate limit", "too many requests", "429", "throttl"]) {
        return ErrorCategory::RateLimit;
    }

    // --- Priority 13: Timeout ---
    if contains_any(&msg, &["timeout", "timed out", "deadline exceeded"]) {
        return ErrorCategory::Timeout;
    }

    // --- Priority 14: Provider transient response-shape failures ---
    if contains_any(
        &msg,
        &[
            "invalid response format: missing choices",
            "invalid response format: missing message",
            "missing choices in response",
            "missing message in choice",
            "no choices in response",
            "invalid response from ",
            "empty response body",
            "response did not contain",
            "unexpected response format",
            "failed to parse response",
        ],
    ) {
        return ErrorCategory::ServiceUnavailable;
    }

    // --- Priority 15: Service unavailable (HTTP 5xx and related) ---
    if contains_any(
        &msg,
        &[
            "service unavailable",
            "temporarily unavailable",
            "internal server error",
            "bad gateway",
            "gateway timeout",
            "overloaded",
            "500",
            "502",
            "503",
            "504",
        ],
    ) {
        return ErrorCategory::ServiceUnavailable;
    }

    // --- Priority 16: Network (connectivity, DNS, transport) ---
    if contains_any(
        &msg,
        &[
            "network",
            "connection reset",
            "connection refused",
            "broken pipe",
            "dns",
            "name resolution",
            "try again",
            "retry later",
            "upstream connect error",
            "tls handshake",
            "socket hang up",
            "econnreset",
            "etimedout",
            // reqwest surfaces truncated/aborted response streams as a body
            // decode failure; Ollama (cloud) emits this on transient drops.
            // It is a transport error, not a payload problem — retryable.
            "error decoding response body",
        ],
    ) {
        return ErrorCategory::Network;
    }

    // --- Priority 17: Resource exhausted (memory, disk) ---
    if contains_any(&msg, &["out of memory", "disk full", "no space left"]) {
        return ErrorCategory::ResourceExhausted;
    }

    // --- Fallback ---
    ErrorCategory::ExecutionError
}

/// Check if an LLM error message is retryable (used by the LLM request retry loop).
///
/// This is a focused classifier for LLM provider errors, combining
/// non-retryable and retryable marker checks for the request retry path.
#[inline]
#[must_use]
pub fn is_retryable_llm_error_message(msg: &str) -> bool {
    let category = classify_error_message(msg);
    category.is_retryable()
}

#[inline]
fn contains_any(message: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| message.contains(marker))
}

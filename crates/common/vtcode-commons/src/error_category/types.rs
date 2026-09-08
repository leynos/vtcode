//! Public taxonomy and retry-policy data types.

use std::time::Duration;

/// Canonical error category used throughout VT Code for consistent
/// retry decisions, user-facing messages, and error handling strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ErrorCategory {
    // === Retryable (Transient) ===
    /// Network connectivity issue (connection reset, DNS failure, etc.)
    Network,
    /// Request timed out or deadline exceeded
    Timeout,
    /// Rate limit exceeded (HTTP 429, provider throttling)
    RateLimit,
    /// External service temporarily unavailable (HTTP 5xx)
    ServiceUnavailable,
    /// Circuit breaker is open for this tool/service
    CircuitOpen,

    // === Non-Retryable (Permanent) ===
    /// Authentication or authorization failure (invalid API key, expired token)
    Authentication,
    /// Invalid parameters, arguments, or schema validation failure
    InvalidParameters,
    /// Tool not found or unavailable
    ToolNotFound,
    /// Resource not found (file, directory, path does not exist)
    ResourceNotFound,
    /// OS-level permission denied (file permissions, EACCES, EPERM)
    PermissionDenied,
    /// Policy violation (workspace boundary, tool deny policy, safety gate)
    PolicyViolation,
    /// Planning workflow violation (mutating tool without read-only capabilities)
    PlanningPolicyViolation,
    /// Sandbox execution failure
    SandboxFailure,
    /// Resource exhausted (quota, billing, spending limit, disk, memory)
    ResourceExhausted,
    /// User cancelled the operation
    Cancelled,
    /// General execution error (catch-all for unclassified failures)
    ExecutionError,
}

/// Describes whether and how an error can be retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retryability {
    /// Error is transient and may succeed on retry.
    Retryable {
        /// Suggested maximum retry attempts.
        max_attempts: u32,
        /// Suggested backoff strategy.
        backoff: BackoffStrategy,
    },
    /// Error is permanent and should NOT be retried.
    NonRetryable,
    /// Error requires human intervention before proceeding.
    RequiresIntervention,
}

/// Backoff strategy for retryable errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackoffStrategy {
    /// Exponential backoff with base delay and maximum cap.
    Exponential { base: Duration, max: Duration },
    /// Fixed delay between retries (e.g., for rate-limited APIs with Retry-After).
    Fixed(Duration),
}

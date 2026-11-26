//! Category predicates, retry policy, and stable display labels.

use std::fmt;
use std::time::Duration;

use super::{BackoffStrategy, ErrorCategory, Retryability};

impl ErrorCategory {
    /// Return the category as a static string without allocating.
    #[inline]
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        self.user_label()
    }

    /// Whether this error category is safe to retry.
    #[inline]
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            ErrorCategory::Network
                | ErrorCategory::Timeout
                | ErrorCategory::RateLimit
                | ErrorCategory::ServiceUnavailable
                | ErrorCategory::CircuitOpen
        )
    }

    /// Whether this category should count toward circuit breaker transitions.
    #[inline]
    #[must_use]
    pub const fn should_trip_circuit_breaker(&self) -> bool {
        matches!(
            self,
            ErrorCategory::Network
                | ErrorCategory::Timeout
                | ErrorCategory::RateLimit
                | ErrorCategory::ServiceUnavailable
                | ErrorCategory::ExecutionError
        )
    }

    /// Whether this error is an LLM argument mistake (should not count toward
    /// circuit breaker thresholds).
    #[inline]
    #[must_use]
    pub(super) const fn is_llm_mistake(&self) -> bool {
        matches!(self, ErrorCategory::InvalidParameters)
    }

    /// Whether this error represents a permanent, non-recoverable condition.
    #[inline]
    #[must_use]
    pub const fn is_permanent(&self) -> bool {
        matches!(
            self,
            ErrorCategory::Authentication
                | ErrorCategory::PolicyViolation
                | ErrorCategory::PlanningPolicyViolation
                | ErrorCategory::ResourceExhausted
        )
    }

    /// Get the recommended retryability for this error category.
    #[must_use]
    pub fn retryability(&self) -> Retryability {
        match self {
            ErrorCategory::Network | ErrorCategory::ServiceUnavailable => Retryability::Retryable {
                max_attempts: 3,
                backoff: BackoffStrategy::Exponential {
                    base: Duration::from_millis(500),
                    max: Duration::from_secs(10),
                },
            },
            ErrorCategory::Timeout => Retryability::Retryable {
                max_attempts: 2,
                backoff: BackoffStrategy::Exponential {
                    base: Duration::from_millis(1000),
                    max: Duration::from_secs(15),
                },
            },
            ErrorCategory::RateLimit => Retryability::Retryable {
                max_attempts: 3,
                backoff: BackoffStrategy::Exponential {
                    base: Duration::from_secs(1),
                    max: Duration::from_secs(30),
                },
            },
            ErrorCategory::CircuitOpen => Retryability::Retryable {
                max_attempts: 1,
                backoff: BackoffStrategy::Fixed(Duration::from_secs(10)),
            },
            ErrorCategory::PermissionDenied => Retryability::RequiresIntervention,
            _ => Retryability::NonRetryable,
        }
    }

    /// Get a concise, user-friendly label for this error category.
    #[must_use]
    pub const fn user_label(&self) -> &'static str {
        match self {
            ErrorCategory::Network => "Network error",
            ErrorCategory::Timeout => "Request timed out",
            ErrorCategory::RateLimit => "Rate limit exceeded",
            ErrorCategory::ServiceUnavailable => "Service temporarily unavailable",
            ErrorCategory::CircuitOpen => "Tool temporarily disabled",
            ErrorCategory::Authentication => "Authentication failed",
            ErrorCategory::InvalidParameters => "Invalid parameters",
            ErrorCategory::ToolNotFound => "Tool not found",
            ErrorCategory::ResourceNotFound => "Resource not found",
            ErrorCategory::PermissionDenied => "Permission denied",
            ErrorCategory::PolicyViolation => "Blocked by policy",
            ErrorCategory::PlanningPolicyViolation => "Not allowed in planning workflow",
            ErrorCategory::SandboxFailure => "Sandbox denied",
            ErrorCategory::ResourceExhausted => "Resource limit reached",
            ErrorCategory::Cancelled => "Operation cancelled",
            ErrorCategory::ExecutionError => "Execution failed",
        }
    }
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.user_label())
    }
}

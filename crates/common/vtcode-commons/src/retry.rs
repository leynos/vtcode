#![expect(
    clippy::cast_possible_truncation,
    reason = "Retry exponents and jitter are clamped to the supported retry range before conversion."
)]

//! Canonical retry policy shared across the workspace.
//!
//! This module owns the retry *policy math*: attempt budgets, exponential
//! backoff with an optional deterministic jitter, and category-based retry
//! decisions built on [`ErrorCategory::is_retryable`]. Domain-specific
//! adapters (typed error downcasts, tool-aware timeout rules, LLM
//! `Retry-After` extraction) live in `vtcode-core::retry` as an extension
//! trait over this policy.
//!
//! Wire-level HTTP clients that only need "should I retry this call?" use
//! [`RetryPolicy::classify_anyhow`] / [`RetryPolicy::classify_status`];
//! richer loops use [`RetryPolicy::decision_for_category`].

use std::time::Duration;

use crate::error_category::{ErrorCategory, classify_anyhow_error};

/// Typed retry policy shared across runtime layers.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of total attempts, including the initial call.
    pub max_attempts: u32,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub multiplier: f64,
    pub jitter: f64,
}

impl RetryPolicy {
    pub const DEFAULT: Self = Self::from_retries(2, Duration::from_secs(1), Duration::from_secs(60), 2.0);

    pub const fn new(max_attempts: u32, initial_delay: Duration, max_delay: Duration, multiplier: f64) -> Self {
        Self {
            max_attempts: if max_attempts < 1 { 1 } else { max_attempts },
            initial_delay,
            max_delay,
            multiplier: if multiplier < 1.0 { 1.0 } else { multiplier },
            jitter: 0.0,
        }
    }

    pub const fn from_retries(max_retries: u32, initial_delay: Duration, max_delay: Duration, multiplier: f64) -> Self {
        Self::new(max_retries.saturating_add(1), initial_delay, max_delay, multiplier)
    }

    /// Millisecond-based constructor for wire clients.
    ///
    /// Uses a 2.0 multiplier and no jitter, so
    /// [`Self::delay_for_attempt`] reproduces the classic
    /// `base_ms << attempt` doubling curve capped at `max_delay_ms`.
    fn simple(max_retries: u32, base_delay_ms: u64, max_delay_ms: u64) -> Self {
        Self::from_retries(max_retries, Duration::from_millis(base_delay_ms), Duration::from_millis(max_delay_ms), 2.0)
    }

    pub fn delay_for_attempt(&self, attempt_index: u32) -> Duration {
        self.delay_for_attempt_with_floor(attempt_index, None)
    }

    /// Calculate the delay for an attempt while respecting a provider's
    /// minimum wait.
    ///
    /// The ordinary policy curve remains capped by [`Self::max_delay`]. When
    /// a provider supplies a positive floor, its curve starts at the larger
    /// of that floor and [`Self::initial_delay`] and grows without being
    /// reduced by the local cap. Jitter is additive, so it can never shorten
    /// either minimum.
    pub fn delay_for_attempt_with_floor(&self, attempt_index: u32, provider_floor: Option<Duration>) -> Duration {
        let policy_delay = exponential_delay(self.initial_delay, self.multiplier, attempt_index).min(self.max_delay);
        let minimum_delay =
            provider_floor
                .filter(|provider_floor| !provider_floor.is_zero())
                .map_or(policy_delay, |provider_floor| {
                    let provider_base = self.initial_delay.max(provider_floor);
                    policy_delay.max(exponential_delay(provider_base, self.multiplier, attempt_index))
                });

        if !self.jitter.is_finite() || self.jitter <= 0.0 {
            return minimum_delay;
        }

        #[expect(
            clippy::cast_sign_loss,
            reason = "Duration milliseconds are non-negative, jitter is positive and finite, and the product is clamped to u64."
        )]
        let max_jitter_ms = (minimum_delay.as_millis() as f64 * self.jitter)
            .round()
            .clamp(0.0, u64::MAX as f64) as u64;
        if max_jitter_ms == 0 {
            return minimum_delay;
        }

        let offset = (u64::from(attempt_index) * 31) % max_jitter_ms.saturating_add(1);
        minimum_delay.saturating_add(Duration::from_millis(offset))
    }

    pub fn decision_for_category(
        &self,
        category: ErrorCategory,
        attempt_index: u32,
        retry_after: Option<Duration>,
    ) -> RetryDecision {
        let has_remaining_attempts = attempt_index.saturating_add(1) < self.max_attempts;
        if !category.is_retryable() || !has_remaining_attempts {
            return RetryDecision {
                category,
                retryable: false,
                delay: None,
                retry_after,
            };
        }

        let delay = self.delay_for_attempt_with_floor(attempt_index, retry_after);
        RetryDecision {
            category,
            retryable: true,
            delay: Some(delay),
            retry_after,
        }
    }

    /// Make a retry decision while retaining provider backoff state across
    /// attempts. Later, larger floors raise the retained minimum; missing or
    /// smaller floors do not lower it.
    pub fn decision_for_category_with_backoff(
        &self,
        category: ErrorCategory,
        attempt_index: u32,
        retry_after: Option<Duration>,
        backoff: &mut RetryBackoff,
    ) -> RetryDecision {
        backoff.observe(retry_after);

        let mut decision = self.decision_for_category(category, attempt_index, None);
        decision.retry_after = retry_after;
        if decision.retryable {
            decision.delay = Some(self.delay_for_attempt_with_floor(attempt_index, backoff.provider_floor));
        }
        decision
    }

    /// Classify an `anyhow::Error` for retry eligibility.
    ///
    /// Attempt-agnostic: `retryable` reflects only the error category, not
    /// the remaining attempt budget. Wire clients that manage their own
    /// attempt counting use this; loops that want budget-aware decisions
    /// use [`Self::decision_for_category`].
    pub fn classify_anyhow(&self, error: &anyhow::Error) -> RetryDecision {
        let category = classify_anyhow_error(error);
        RetryDecision {
            category,
            retryable: category.is_retryable(),
            delay: None,
            retry_after: None,
        }
    }

    /// Classify an HTTP status code for retry eligibility.
    ///
    /// Attempt-agnostic, like [`Self::classify_anyhow`].
    pub fn classify_status(&self, status: u16) -> RetryDecision {
        let category = match status {
            429 => ErrorCategory::RateLimit,
            500 | 502 | 504 => ErrorCategory::Network,
            503 => ErrorCategory::ServiceUnavailable,
            401 | 403 => ErrorCategory::Authentication,
            _ => ErrorCategory::ExecutionError,
        };
        RetryDecision {
            category,
            retryable: category.is_retryable(),
            delay: None,
            retry_after: None,
        }
    }
}

fn exponential_delay(base: Duration, multiplier: f64, attempt_index: u32) -> Duration {
    let exponent = i32::try_from(attempt_index).unwrap_or(i32::MAX);
    let multiplier = if multiplier.is_finite() && multiplier >= 1.0 {
        multiplier
    } else {
        1.0
    };
    Duration::try_from_secs_f64(base.as_secs_f64() * multiplier.powi(exponent))
        .unwrap_or(Duration::MAX)
        .max(base)
}

/// Provider-supplied retry floor retained for one logical retry segment.
///
/// Reuse this value across the attempts belonging to one generation or tool
/// segment, then call [`Self::reset`] before starting the next segment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RetryBackoff {
    provider_floor: Option<Duration>,
}

impl RetryBackoff {
    /// Create empty backoff state for a new logical retry segment.
    #[must_use]
    pub const fn new() -> Self {
        Self { provider_floor: None }
    }

    /// Return the largest provider floor observed in this segment.
    #[must_use]
    pub const fn provider_floor(&self) -> Option<Duration> {
        self.provider_floor
    }

    /// Clear the retained floor before a new generation or tool segment.
    pub fn reset(&mut self) {
        self.provider_floor = None;
    }

    fn observe(&mut self, retry_after: Option<Duration>) {
        if let Some(retry_after) = retry_after {
            self.provider_floor = Some(self.provider_floor.map_or(retry_after, |floor| floor.max(retry_after)));
        }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::from_retries(2, Duration::from_secs(1), Duration::from_secs(60), 2.0)
    }
}

/// Result of classifying a failure for retry handling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryDecision {
    pub category: ErrorCategory,
    pub retryable: bool,
    pub delay: Option<Duration>,
    pub retry_after: Option<Duration>,
}

#[cfg(test)]
mod tests;

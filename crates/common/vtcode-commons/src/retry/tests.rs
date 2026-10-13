//! Tests shared retry-policy arithmetic, classification, and backoff state.

use super::*;
use proptest::prelude::*;

struct StatusCase {
    name: &'static str,
    status: u16,
    category: ErrorCategory,
    retryable: bool,
}

#[test]
fn default_policy_allows_two_retries() {
    let policy = RetryPolicy::default();
    assert_eq!(policy.max_attempts, 3);
    assert_eq!(policy.initial_delay, Duration::from_secs(1));
    assert_eq!(policy.max_delay, Duration::from_secs(60));
}

#[test]
fn classify_status_cases() {
    for StatusCase { name, status, category, retryable } in [
        StatusCase {
            name: "rate_limit",
            status: 429,
            category: ErrorCategory::RateLimit,
            retryable: true,
        },
        StatusCase {
            name: "server_error",
            status: 503,
            category: ErrorCategory::ServiceUnavailable,
            retryable: true,
        },
        StatusCase {
            name: "auth_not_retryable",
            status: 401,
            category: ErrorCategory::Authentication,
            retryable: false,
        },
    ] {
        let decision = RetryPolicy::default().classify_status(status);
        assert_eq!(decision.category, category, "{name} category");
        assert_eq!(decision.retryable, retryable, "{name} retryability");
    }
}

#[test]
fn classify_anyhow_network_error() {
    let policy = RetryPolicy::default();
    let err = anyhow::anyhow!("connection refused");
    let decision = policy.classify_anyhow(&err);
    assert!(decision.retryable);
}

#[test]
fn simple_policy_matches_bit_shift_doubling() {
    // Parity with the historical `base_ms << attempt` curve used by
    // wire clients before consolidation.
    let policy = RetryPolicy::simple(10, 1000, 5000);
    let legacy = |attempt: u32| -> u64 { 1000u64.saturating_mul(1u64 << attempt.min(16)).min(5000) };
    for attempt in 0..6 {
        assert_eq!(
            policy.delay_for_attempt(attempt),
            Duration::from_millis(legacy(attempt)),
            "delay mismatch at attempt {attempt}"
        );
    }
}

#[test]
fn delay_for_attempt_clamps_overflowing_backoff_to_max_delay() {
    let policy = RetryPolicy::from_retries(3, Duration::from_secs(1), Duration::from_secs(8), f64::MAX);

    assert_eq!(policy.delay_for_attempt(2), Duration::from_secs(8));
}

#[test]
fn delay_for_attempt_ignores_non_finite_jitter() {
    let mut policy = RetryPolicy::from_retries(3, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    policy.jitter = f64::INFINITY;

    assert_eq!(policy.delay_for_attempt(1), Duration::from_secs(2));
}

#[test]
fn delay_for_attempt_handles_huge_finite_jitter() {
    let mut policy = RetryPolicy::from_retries(3, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    policy.jitter = f64::MAX;

    assert!(policy.delay_for_attempt(1) >= Duration::from_secs(2));
}

#[test]
fn decision_for_category_respects_attempt_budget() {
    let policy = RetryPolicy::from_retries(1, Duration::from_secs(1), Duration::from_secs(8), 2.0);

    let first = policy.decision_for_category(ErrorCategory::Network, 0, None);
    assert!(first.retryable);
    assert_eq!(first.delay, Some(Duration::from_secs(1)));

    let exhausted = policy.decision_for_category(ErrorCategory::Network, 1, None);
    assert!(!exhausted.retryable);
    assert!(exhausted.delay.is_none());
}

#[test]
fn decision_for_category_prefers_retry_after() {
    let policy = RetryPolicy::from_retries(3, Duration::from_secs(1), Duration::from_secs(8), 2.0);

    let decision = policy.decision_for_category(ErrorCategory::RateLimit, 0, Some(Duration::from_secs(7)));
    assert!(decision.retryable);
    assert_eq!(decision.delay, Some(Duration::from_secs(7)));
    assert_eq!(decision.retry_after, Some(Duration::from_secs(7)));
}

#[test]
fn provider_floor_curve_doubles_beyond_local_cap() {
    let policy = RetryPolicy::from_retries(4, Duration::from_secs(1), Duration::from_secs(8), 2.0);

    assert_eq!(policy.delay_for_attempt_with_floor(0, Some(Duration::from_secs(7))), Duration::from_secs(7));
    assert_eq!(policy.delay_for_attempt_with_floor(1, Some(Duration::from_secs(7))), Duration::from_secs(14));
    assert_eq!(policy.delay_for_attempt_with_floor(2, Some(Duration::from_secs(7))), Duration::from_secs(28));
}

#[test]
fn provider_floor_never_reduces_the_initial_delay() {
    let policy = RetryPolicy::from_retries(3, Duration::from_secs(5), Duration::from_secs(30), 2.0);

    assert_eq!(policy.delay_for_attempt_with_floor(1, Some(Duration::from_secs(2))), Duration::from_secs(10));
    assert_eq!(policy.delay_for_attempt_with_floor(4, Some(Duration::from_secs(2))), Duration::from_secs(80));
}

#[test]
fn zero_provider_floor_keeps_the_ordinary_cap() {
    let policy = RetryPolicy::from_retries(6, Duration::from_secs(1), Duration::from_secs(8), 2.0);

    assert_eq!(policy.delay_for_attempt_with_floor(5, Some(Duration::ZERO)), Duration::from_secs(8));
}

#[test]
fn retry_backoff_remembers_provider_floor_until_reset() {
    let policy = RetryPolicy::from_retries(4, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    let mut backoff = RetryBackoff::new();

    let first = policy.decision_for_category_with_backoff(
        ErrorCategory::RateLimit,
        0,
        Some(Duration::from_secs(7)),
        &mut backoff,
    );
    let second = policy.decision_for_category_with_backoff(ErrorCategory::RateLimit, 1, None, &mut backoff);

    assert_eq!(first.delay, Some(Duration::from_secs(7)));
    assert_eq!(second.delay, Some(Duration::from_secs(14)));
    assert_eq!(second.retry_after, None);
    assert_eq!(backoff.provider_floor(), Some(Duration::from_secs(7)));

    backoff.reset();
    let after_reset = policy.decision_for_category_with_backoff(ErrorCategory::RateLimit, 2, None, &mut backoff);
    assert_eq!(after_reset.delay, Some(Duration::from_secs(4)));
    assert_eq!(backoff.provider_floor(), None);
}

#[test]
fn additive_jitter_never_undercuts_provider_curve() {
    let mut policy = RetryPolicy::from_retries(3, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    policy.jitter = 0.5;

    assert!(policy.delay_for_attempt_with_floor(1, Some(Duration::from_secs(7))) >= Duration::from_secs(14));
}

#[test]
fn huge_provider_floor_and_attempt_saturate_without_panicking() {
    let policy = RetryPolicy::from_retries(u32::MAX, Duration::from_secs(1), Duration::from_secs(8), f64::MAX);

    assert_eq!(policy.delay_for_attempt_with_floor(u32::MAX, Some(Duration::MAX)), Duration::MAX);
}

#[test]
fn non_finite_multiplier_does_not_produce_an_invalid_duration() {
    let mut policy = RetryPolicy::from_retries(3, Duration::from_secs(2), Duration::from_secs(8), 2.0);
    policy.multiplier = f64::NAN;

    assert_eq!(policy.delay_for_attempt(u32::MAX), Duration::from_secs(2));
}

proptest! {
    #[test]
    fn provider_floor_curve_is_monotonic_and_never_undercut(
        initial_delay_ms in 0_u64..=1_000_000,
        max_delay_ms in 0_u64..=1_000_000,
        provider_floor_ms in 0_u64..=1_000_000,
        attempt_index in 0_u32..=64,
    ) {
        let policy = RetryPolicy::from_retries(
            u32::MAX,
            Duration::from_millis(initial_delay_ms),
            Duration::from_millis(max_delay_ms),
            2.0,
        );
        let provider_floor = Duration::from_millis(provider_floor_ms);
        let delay = policy.delay_for_attempt_with_floor(attempt_index, Some(provider_floor));
        let next_delay = policy.delay_for_attempt_with_floor(
            attempt_index.saturating_add(1),
            Some(provider_floor),
        );

        prop_assert!(delay >= provider_floor);
        prop_assert!(next_delay >= delay);
    }
}

#[test]
fn exhausted_budget_never_schedules_a_delay() {
    let policy = RetryPolicy::from_retries(1, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    let mut backoff = RetryBackoff::new();

    let exhausted = policy.decision_for_category_with_backoff(
        ErrorCategory::RateLimit,
        1,
        Some(Duration::from_secs(120)),
        &mut backoff,
    );

    assert!(!exhausted.retryable);
    assert_eq!(exhausted.delay, None);
    assert_eq!(exhausted.retry_after, Some(Duration::from_secs(120)));
}

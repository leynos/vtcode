//! Tests timestamp-aware retry decisions and retry execution behaviour.

use super::*;
use crate::config::constants::tools;
use crate::error::{ErrorCode, VtCodeError};

#[test]
fn non_retryable_categories_stop_immediately() {
    let policy = RetryPolicy::from_retries(2, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    let err = VtCodeError::security(ErrorCode::PermissionDenied, "blocked by policy");

    let decision = policy.decision_for_vtcode_error(&err, 0, None);
    assert_eq!(decision.category, ErrorCategory::PolicyViolation);
    assert!(!decision.retryable);
    assert!(decision.delay.is_none());
}

/// Rate-limit failure carrying the provider's advertised wait, if any.
fn rate_limit_error(retry_after: Option<&str>) -> LLMError {
    LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "Anthropic",
            Some(429),
            Some("rate_limit_error".to_string()),
            None,
            None,
            retry_after.map(str::to_string),
            Some("too many requests".to_string()),
        )),
    }
}

fn floor_policy() -> RetryPolicy {
    RetryPolicy::from_retries(3, Duration::from_secs(1), Duration::from_secs(8), 2.0)
}

#[test]
fn retry_after_header_overrides_backoff_delay_for_typed_and_wrapped_errors() {
    let policy = floor_policy();
    let typed = policy.decision_for_llm_error(&rate_limit_error(Some("7")), 0);
    let wrapped = policy.decision_for_anyhow(&anyhow::Error::new(rate_limit_error(Some("7"))), 0, None);
    let expected = (true, Some(Duration::from_secs(7)), Some(Duration::from_secs(7)));

    for (label, decision) in [("typed", typed), ("wrapped", wrapped)] {
        assert_eq!((decision.retryable, decision.retry_after, decision.delay), expected, "{label}");
    }
}

#[test]
fn retry_backoff_retains_floor_when_later_provider_error_omits_header() {
    let policy = floor_policy();
    let mut backoff = RetryBackoff::new();

    let first = policy.decision_for_llm_error_with_backoff_at(
        &rate_limit_error(Some("7")),
        LlmRetryAttempt::now(0),
        &mut backoff,
    );
    let later =
        policy.decision_for_llm_error_with_backoff_at(&rate_limit_error(None), LlmRetryAttempt::now(1), &mut backoff);

    assert_eq!(
        (first.delay, later.retry_after, later.delay),
        (Some(Duration::from_secs(7)), None, Some(Duration::from_secs(14)))
    );
}

#[test]
fn retry_backoff_does_not_retain_an_http_date_floor() {
    let policy = floor_policy();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_445_412_420);
    let retry_date = httpdate::fmt_http_date(now + Duration::from_secs(60));
    let mut backoff = RetryBackoff::new();

    let first = policy.decision_for_llm_error_with_backoff_at(
        &rate_limit_error(Some(&retry_date)),
        LlmRetryAttempt::new(0, now),
        &mut backoff,
    );
    let later = policy.decision_for_llm_error_with_backoff_at(
        &rate_limit_error(None),
        LlmRetryAttempt::new(1, now + Duration::from_secs(61)),
        &mut backoff,
    );

    assert_eq!(
        (first.retry_after, first.delay, backoff.provider_floor()),
        (Some(Duration::from_secs(60)), Some(Duration::from_secs(60)), None)
    );
    assert_eq!((later.retry_after, later.delay), (None, Some(Duration::from_secs(2))));
}

#[test]
fn quota_exhaustion_is_not_retryable() {
    let policy = RetryPolicy::from_retries(3, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    let err = LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "OpenAI",
            Some(429),
            Some("insufficient_quota".to_string()),
            None,
            None,
            None,
            Some("quota exceeded".to_string()),
        )),
    };

    let decision = policy.decision_for_llm_error(&err, 0);
    assert_eq!(decision.category, ErrorCategory::ResourceExhausted);
    assert!(!decision.retryable);
}

fn quota_exhausted_error() -> LLMError {
    LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "OpenAI",
            Some(429),
            Some("insufficient_quota".to_string()),
            None,
            None,
            None,
            Some("quota exceeded".to_string()),
        )),
    }
}

#[test]
fn clock_free_retry_eligibility_matches_anyhow_decisions() {
    let policy = floor_policy();
    let llm_errors = [
        rate_limit_error(Some("7")),
        rate_limit_error(None),
        quota_exhausted_error(),
        LLMError::Network {
            message: "temporary disconnect".to_string(),
            metadata: None,
        },
        LLMError::InvalidRequest { message: "bad request".to_string(), metadata: None },
    ];

    for error in llm_errors {
        let wrapped = anyhow::Error::new(error);
        assert_eq!(
            policy.is_retryable_anyhow(&wrapped, 0, None),
            policy.decision_for_anyhow(&wrapped, 0, None).retryable,
            "clock-free eligibility must match the typed anyhow decision"
        );
    }

    let typed_vtcode = anyhow::Error::new(VtCodeError::network(ErrorCode::ConnectionFailed, "transient"));
    assert!(policy.is_retryable_anyhow(&typed_vtcode, 0, None));
    assert_eq!(
        policy.is_retryable_anyhow(&typed_vtcode, 0, None),
        policy.decision_for_anyhow(&typed_vtcode, 0, None).retryable
    );
}

#[test]
fn retry_eligibility_ignores_future_http_date_retry_after() {
    let policy = floor_policy();
    let future = SystemTime::now() + Duration::from_secs(3_600);
    let with_date = anyhow::Error::new(rate_limit_error(Some(&httpdate::fmt_http_date(future))));
    let without_date = anyhow::Error::new(rate_limit_error(None));

    assert_eq!(
        policy.is_retryable_anyhow(&with_date, 0, None),
        policy.decision_for_anyhow(&with_date, 0, None).retryable
    );
    assert_eq!(
        policy.is_retryable_anyhow(&with_date, 0, None),
        policy.is_retryable_anyhow(&without_date, 0, None),
        "eligibility must not depend on an absolute Retry-After date"
    );
    assert!(policy.is_retryable_anyhow(&with_date, 0, None));
}

#[test]
fn anyhow_fallback_uses_shared_classifier() {
    let policy = RetryPolicy::from_retries(1, Duration::from_secs(1), Duration::from_secs(8), 2.0);

    let decision = policy.decision_for_anyhow(&anyhow::anyhow!("HTTP 503 Service Unavailable"), 0, None);
    assert_eq!(decision.category, ErrorCategory::ServiceUnavailable);
    assert!(decision.retryable);
    assert_eq!(decision.delay, Some(Duration::from_secs(1)));
}

#[test]
fn canonical_exec_aliases_are_command_tools() {
    for alias in [
        tools::RUN_PTY_CMD,
        tools::EXEC_COMMAND,
        tools::WRITE_STDIN,
        tools::UNIFIED_EXEC,
        "shell",
        "bash",
        "container.exec",
    ] {
        assert!(is_command_tool(alias), "expected {alias} to be a command tool");
    }
}

#[test]
fn typed_tool_timeout_for_command_tools_is_not_retryable() {
    let policy = RetryPolicy::from_retries(2, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    let err = UnifiedToolError::new(crate::tools::unified_error::UnifiedErrorKind::Timeout, "timed out")
        .with_tool_name(tools::RUN_PTY_CMD);

    let decision = policy.decision_for_tool_error(&err, 0);
    assert_eq!(decision.category, ErrorCategory::Timeout);
    assert!(!decision.retryable);
}

#[test]
fn anyhow_typed_tool_timeout_uses_fallback_tool_name() {
    let policy = RetryPolicy::from_retries(2, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    let err =
        anyhow::Error::new(UnifiedToolError::new(crate::tools::unified_error::UnifiedErrorKind::Timeout, "timed out"));

    let decision = policy.decision_for_anyhow(&err, 0, Some(tools::RUN_PTY_CMD));
    assert_eq!(decision.category, ErrorCategory::Timeout);
    assert!(!decision.retryable);
}

#[test]
fn command_timeouts_do_not_retry() {
    let policy = RetryPolicy::from_retries(2, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    let err = VtCodeError::new(ErrorCategory::Timeout, ErrorCode::Timeout, "timed out");

    let decision = policy.decision_for_vtcode_error(&err, 0, Some(tools::RUN_PTY_CMD));
    assert_eq!(decision.category, ErrorCategory::Timeout);
    assert!(!decision.retryable);
}

#[tokio::test]
async fn run_with_retry_returns_first_success() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    let policy = RetryPolicy::from_retries(3, Duration::from_millis(0), Duration::from_millis(1), 2.0);
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_for_op = attempts.clone();
    let result: crate::error::Result<String> = run_with_retry(
        &policy,
        &mut (),
        |_: &mut (), _| {},
        |_| {
            let attempts = attempts_for_op.clone();
            Box::pin(async move {
                let n = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                if n < 2 {
                    Err(VtCodeError::network(ErrorCode::ConnectionFailed, "transient"))
                } else {
                    Ok("ok".to_string())
                }
            })
        },
        |_: &RetryPolicy| VtCodeError::execution(ErrorCode::ToolExecutionFailed, "exhausted"),
    )
    .await;
    assert_eq!(result.expect("retry should return the first successful result"), "ok");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn run_with_retry_surfaces_give_up_immediately() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    let policy = RetryPolicy::from_retries(5, Duration::from_millis(0), Duration::from_millis(1), 2.0);
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_for_op = attempts.clone();
    let result: crate::error::Result<String> = run_with_retry(
        &policy,
        &mut (),
        |_: &mut (), _| {},
        |_| {
            let attempts = attempts_for_op.clone();
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<String, _>(VtCodeError::input(ErrorCode::InvalidArgument, "bad input"))
            })
        },
        |_: &RetryPolicy| VtCodeError::execution(ErrorCode::ToolExecutionFailed, "exhausted"),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(attempts.load(Ordering::SeqCst), 1, "GiveUp should short-circuit retries");
}

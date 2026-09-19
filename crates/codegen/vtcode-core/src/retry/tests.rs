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

#[test]
fn retry_after_header_overrides_backoff_delay() {
    let policy = RetryPolicy::from_retries(3, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    let err = LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "Anthropic",
            Some(429),
            Some("rate_limit_error".to_string()),
            None,
            None,
            Some("7".to_string()),
            Some("too many requests".to_string()),
        )),
    };

    let decision = policy.decision_for_llm_error(&err, 0);
    assert!(decision.retryable);
    assert_eq!(decision.retry_after, Some(Duration::from_secs(7)));
    assert_eq!(decision.delay, Some(Duration::from_secs(7)));
}

#[test]
fn retry_backoff_retains_floor_when_later_provider_error_omits_header() {
    let policy = RetryPolicy::from_retries(3, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    let first_error = LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "Anthropic",
            Some(429),
            Some("rate_limit_error".to_string()),
            None,
            None,
            Some("7".to_string()),
            Some("too many requests".to_string()),
        )),
    };
    let later_error = LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "Anthropic",
            Some(429),
            Some("rate_limit_error".to_string()),
            None,
            None,
            None,
            Some("too many requests".to_string()),
        )),
    };
    let mut backoff = RetryBackoff::new();

    let first = policy.decision_for_llm_error_with_backoff(&first_error, 0, &mut backoff);
    let later = policy.decision_for_llm_error_with_backoff(&later_error, 1, &mut backoff);

    assert_eq!(first.delay, Some(Duration::from_secs(7)));
    assert_eq!(later.retry_after, None);
    assert_eq!(later.delay, Some(Duration::from_secs(14)));
}

#[test]
fn retry_backoff_does_not_retain_an_http_date_floor() {
    let policy = RetryPolicy::from_retries(3, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_445_412_420);
    let retry_date = httpdate::fmt_http_date(now + Duration::from_secs(60));
    let first_error = LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "Anthropic",
            Some(429),
            Some("rate_limit_error".to_string()),
            None,
            None,
            Some(retry_date),
            Some("too many requests".to_string()),
        )),
    };
    let later_error = LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "Anthropic",
            Some(429),
            Some("rate_limit_error".to_string()),
            None,
            None,
            None,
            Some("too many requests".to_string()),
        )),
    };
    let mut backoff = RetryBackoff::new();

    let first = policy.decision_for_llm_error_with_backoff_at(&first_error, 0, &mut backoff, now);
    let later =
        policy.decision_for_llm_error_with_backoff_at(&later_error, 1, &mut backoff, now + Duration::from_secs(61));

    assert_eq!(first.retry_after, Some(Duration::from_secs(60)));
    assert_eq!(first.delay, Some(Duration::from_secs(60)));
    assert_eq!(backoff.provider_floor(), None);
    assert_eq!(later.retry_after, None);
    assert_eq!(later.delay, Some(Duration::from_secs(2)));
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

#[test]
fn anyhow_fallback_uses_shared_classifier() {
    let policy = RetryPolicy::from_retries(1, Duration::from_secs(1), Duration::from_secs(8), 2.0);

    let decision = policy.decision_for_anyhow(&anyhow::anyhow!("HTTP 503 Service Unavailable"), 0, None);
    assert_eq!(decision.category, ErrorCategory::ServiceUnavailable);
    assert!(decision.retryable);
    assert_eq!(decision.delay, Some(Duration::from_secs(1)));
}

#[test]
fn anyhow_prefers_typed_llm_errors() {
    let policy = RetryPolicy::from_retries(3, Duration::from_secs(1), Duration::from_secs(8), 2.0);
    let err = anyhow::Error::new(LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "Anthropic",
            Some(429),
            Some("rate_limit_error".to_string()),
            None,
            None,
            Some("9".to_string()),
            Some("too many requests".to_string()),
        )),
    });

    let decision = policy.decision_for_anyhow(&err, 0, None);
    assert!(decision.retryable);
    assert_eq!(decision.retry_after, Some(Duration::from_secs(9)));
    assert_eq!(decision.delay, Some(Duration::from_secs(9)));
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
    assert_eq!(result.unwrap(), "ok");
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

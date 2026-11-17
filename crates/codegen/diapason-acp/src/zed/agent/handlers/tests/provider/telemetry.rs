//! Telemetry assertions for provider failures.
//!
//! These cases pin the bounded telemetry struct and the warning that records
//! it: a provider's free-form code, status, and message must map to stable
//! fields without carrying payload, URLs, or credentials into the log.

use super::*;

/// Assert network error metadata maps to stable telemetry fields.
#[test]
fn provider_error_telemetry_exposes_stable_network_metadata() {
    let error = LLMError::Network {
        message: "request failed".to_string(),
        metadata: Some(LLMErrorMetadata::new(
            "Arli AI",
            Some(504),
            Some("reqwest_timeout_error".to_string()),
            None,
            None,
            None,
            Some("operation timed out".to_string()),
        )),
    };

    assert_eq!(
        provider_error_telemetry(&error),
        ProviderErrorTelemetry {
            category: "Network error",
            code: Some("reqwest_timeout_error"),
            status: Some(504),
            timeout_phase: None,
        }
    );
}

/// Assert ACP timeout phases surface as their own telemetry field.
#[test]
fn provider_error_telemetry_exposes_acp_timeout_phase_separately() {
    let error = provider_timeout_error("wire-test", StreamTimeoutPhase::InterTokenIdle, Some(Duration::from_secs(30)));

    assert_eq!(
        provider_error_telemetry(&error),
        ProviderErrorTelemetry {
            category: "Network error",
            code: None,
            status: None,
            timeout_phase: Some("inter_token_idle"),
        }
    );
    assert!(error.to_string().contains("inter-token idle timeout"));
}

/// Assert rate-limit metadata maps to telemetry diagnostics.
#[test]
fn provider_error_telemetry_exposes_rate_limit_diagnostics() {
    let error = LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "baseten",
            Some(429),
            Some("rate_limit_error".to_string()),
            None,
            None,
            Some("17".to_string()),
            Some("capacity temporarily exhausted".to_string()),
        )),
    };

    assert_eq!(
        provider_error_telemetry(&error),
        ProviderErrorTelemetry {
            category: "Rate limit exceeded",
            code: Some("rate_limit_error"),
            status: Some(429),
            timeout_phase: None,
        }
    );
}

/// Assert an untrusted provider code is reduced to the bounded fallback.
///
/// `code` is free-form metadata, so an adversarial or newly added provider
/// value must not reach the log verbatim or widen the field's cardinality.
#[test]
fn provider_error_telemetry_bounds_untrusted_codes() {
    let detail = "token=sk-live-secret POST https://api.example.invalid/v1 alice@example.com";
    let error = LLMError::Network {
        message: "request failed".to_string(),
        metadata: Some(LLMErrorMetadata::new(
            "Arli AI",
            Some(500),
            Some(detail.to_string()),
            None,
            None,
            None,
            Some(detail.to_string()),
        )),
    };

    let telemetry = provider_error_telemetry(&error);
    assert_eq!(telemetry.code, Some("other"));
    assert_eq!(telemetry.status, Some(500));

    let rendered = format!("{telemetry:?}");
    for leaked in ["sk-live-secret", "api.example.invalid", "alice@example.com"] {
        assert!(!rendered.contains(leaked), "{leaked} must stay out of the telemetry: {rendered}");
    }
}

/// Assert the emitted provider warning bounds an untrusted code.
///
/// The telemetry struct is bounded by construction, but operators read the
/// warning that `failed` emits. Capture that warning so the allowlist fallback
/// and the absence of the provider metadata are asserted at the boundary that
/// actually writes the record, not only on the intermediate value.
#[test]
fn provider_failure_warning_bounds_untrusted_codes() {
    let detail = "token=sk-live-secret POST https://api.example.invalid/v1 alice@example.com";
    let error = LLMError::Network {
        message: detail.to_string(),
        metadata: Some(LLMErrorMetadata::new(
            "Arli AI",
            Some(500),
            Some(detail.to_string()),
            None,
            None,
            None,
            Some(detail.to_string()),
        )),
    };

    let captured = capture_trace(async {
        let runtime = retry_runtime();
        let metric_sink = ProviderMetricSink::test(Arc::new(TestPerfRecorder::new()));
        let mut telemetry = GenerationTelemetry::start(&runtime, metric_sink, ProviderMode::Buffered);
        telemetry
            .failed(GenerationFailure {
                runtime: &runtime,
                retry_count: 0,
                retry_disposition: RetryDisposition::NonRetryable,
                retry_delay: None,
                error: &error,
            })
            .await;
    });
    assert!(
        captured.contains("ACP provider generation attempt failed"),
        "the warning must be emitted: {captured}"
    );
    assert!(
        captured.contains("provider_error_code=Some(\"other\")"),
        "the code must fall back to the bounded value: {captured}"
    );
    for leaked in ["sk-live-secret", "api.example.invalid", "alice@example.com", "Arli AI"] {
        assert!(!captured.contains(leaked), "{leaked} must stay out of the warning: {captured}");
    }
}

/// Assert every allowlisted code survives unchanged.
#[test]
fn provider_error_telemetry_preserves_allowlisted_codes() {
    for code in KNOWN_ERROR_CODES {
        let error = LLMError::Network {
            message: "request failed".to_string(),
            metadata: Some(LLMErrorMetadata::new(
                "Arli AI",
                Some(503),
                Some((*code).to_string()),
                None,
                None,
                None,
                None,
            )),
        };

        let telemetry = provider_error_telemetry(&error);
        if code.starts_with("acp_timeout_") {
            assert!(telemetry.timeout_phase.is_some(), "{code} must be reported as its timeout phase");
            assert_eq!(telemetry.code, None, "a timeout code must not also be reported as a code");
        } else {
            assert_eq!(telemetry.code, Some(*code), "{code} must be recognised");
        }
    }
}

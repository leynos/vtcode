//! Request-policy tests for custom provider configuration.
use crate::core::custom_provider::{CustomProviderConfig, CustomProviderRequestPolicyConfig};

#[test]
fn request_policy_defaults_allow_five_exponential_retry_delays() {
    let policy = CustomProviderRequestPolicyConfig::default();

    assert_eq!(policy.retry_initial_backoff_ms, 10_000);
    assert_eq!(policy.retry_max_backoff_ms, 160_000);
}

#[test]
fn request_policy_deserializes_provider_limits_and_retries() {
    let parsed: CustomProviderConfig = toml::from_str(
        r#"
name = "mycorp"
display_name = "MyCorp"
base_url = "https://llm.example/v1"
model = "gpt-5-mini"

[request_policy]
max_in_flight_requests = 3
queue_timeout_seconds = 120
max_retries = 4
retry_initial_backoff_ms = 250
retry_max_backoff_ms = 5000
retry_jitter = false
connect_timeout_seconds = 45
first_token_timeout_seconds = 240
stream_idle_timeout_seconds = 150
total_generation_timeout_seconds = 900
"#,
    )
    .expect("custom provider request policy should parse");

    assert_eq!(parsed.request_policy.max_in_flight_requests, Some(3));
    assert_eq!(parsed.request_policy.queue_timeout_seconds, 120);
    assert_eq!(parsed.request_policy.max_retries, 4);
    assert_eq!(parsed.request_policy.retry_initial_backoff_ms, 250);
    assert_eq!(parsed.request_policy.retry_max_backoff_ms, 5_000);
    assert!(!parsed.request_policy.retry_jitter);
    assert_eq!(parsed.request_policy.connect_timeout_seconds, 45);
    assert_eq!(parsed.request_policy.first_token_timeout_seconds, 240);
    assert_eq!(parsed.request_policy.stream_idle_timeout_seconds, 150);
    assert_eq!(parsed.request_policy.total_generation_timeout_seconds, 900);
    assert!(parsed.validate().is_ok());
}

#[test]
fn request_policy_validation_rejects_unsafe_bounds() {
    let mut config = CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorp".to_string(),
        base_url: "https://llm.example/v1".to_string(),
        model: "gpt-5-mini".to_string(),
        ..CustomProviderConfig::default()
    };

    config.request_policy.max_in_flight_requests = Some(0);
    assert!(config.validate().is_err_and(|error| error.contains("max_in_flight_requests")));

    config.request_policy.max_in_flight_requests = Some(usize::MAX);
    assert!(config.validate().is_err_and(|error| error.contains("max_in_flight_requests")));

    config.request_policy.max_in_flight_requests = Some(1);
    config.request_policy.retry_initial_backoff_ms = 2_000;
    config.request_policy.retry_max_backoff_ms = 1_000;
    assert!(config.validate().is_err_and(|error| error.contains("retry_max_backoff_ms")));
}

#[test]
fn deserialize_rejects_invalid_api_format() {
    let err = toml::from_str::<CustomProviderConfig>(
        r#"
name = "mycorp"
display_name = "MyCorp"
base_url = "https://llm.example/v1"
api_format = "openai-chatty"
"#,
    )
    .expect_err("invalid api_format should fail");

    assert!(err.to_string().contains("openai-chatty"));
}

//! Runtime admission and retry policy for custom providers.
use serde::{Deserialize, Serialize};

const fn default_provider_queue_timeout_seconds() -> u64 {
    600
}

const fn default_provider_max_retries() -> u32 {
    2
}

const fn default_provider_retry_initial_backoff_ms() -> u64 {
    10_000
}

const fn default_provider_retry_max_backoff_ms() -> u64 {
    160_000
}

const fn default_provider_retry_jitter() -> bool {
    true
}

const fn default_provider_connect_timeout_seconds() -> u64 {
    30
}

const fn default_provider_first_token_timeout_seconds() -> u64 {
    180
}

const fn default_provider_stream_idle_timeout_seconds() -> u64 {
    120
}

const fn default_provider_total_generation_timeout_seconds() -> u64 {
    600
}

/// Runtime admission and retry policy for a custom provider.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct CustomProviderRequestPolicyConfig {
    /// Maximum provider requests that one Diapason process may keep in flight.
    /// `None` leaves concurrency unrestricted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_in_flight_requests: Option<usize>,

    /// Maximum time to wait for an in-process provider permit.
    #[serde(default = "default_provider_queue_timeout_seconds")]
    pub queue_timeout_seconds: u64,

    /// Number of retries after the initial request for transient failures.
    #[serde(default = "default_provider_max_retries")]
    pub max_retries: u32,

    /// Initial retry backoff duration in milliseconds.
    #[serde(default = "default_provider_retry_initial_backoff_ms")]
    pub retry_initial_backoff_ms: u64,

    /// Maximum retry backoff duration in milliseconds.
    #[serde(default = "default_provider_retry_max_backoff_ms")]
    pub retry_max_backoff_ms: u64,

    /// Add deterministic jitter to retry delays to avoid synchronized reconnects.
    #[serde(default = "default_provider_retry_jitter")]
    pub retry_jitter: bool,

    /// Maximum time to establish the provider connection. Zero disables the limit.
    #[serde(default = "default_provider_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,

    /// Maximum time to wait for the first streamed event. Zero disables the limit.
    #[serde(default = "default_provider_first_token_timeout_seconds")]
    pub first_token_timeout_seconds: u64,

    /// Maximum time between streamed events. Zero disables the limit.
    #[serde(default = "default_provider_stream_idle_timeout_seconds")]
    pub stream_idle_timeout_seconds: u64,

    /// Maximum duration of one provider generation attempt. Zero disables the limit.
    #[serde(default = "default_provider_total_generation_timeout_seconds")]
    pub total_generation_timeout_seconds: u64,
}

impl Default for CustomProviderRequestPolicyConfig {
    fn default() -> Self {
        Self {
            max_in_flight_requests: None,
            queue_timeout_seconds: default_provider_queue_timeout_seconds(),
            max_retries: default_provider_max_retries(),
            retry_initial_backoff_ms: default_provider_retry_initial_backoff_ms(),
            retry_max_backoff_ms: default_provider_retry_max_backoff_ms(),
            retry_jitter: default_provider_retry_jitter(),
            connect_timeout_seconds: default_provider_connect_timeout_seconds(),
            first_token_timeout_seconds: default_provider_first_token_timeout_seconds(),
            stream_idle_timeout_seconds: default_provider_stream_idle_timeout_seconds(),
            total_generation_timeout_seconds: default_provider_total_generation_timeout_seconds(),
        }
    }
}

/// Mirror of Tokio's `Semaphore::MAX_PERMITS`.
///
/// `diapason-config` does not depend on Tokio, but the admission semaphore is
/// constructed from this bound, so validating it here keeps an invalid setting
/// a named configuration error instead of a panic at provider construction.
const MAX_IN_FLIGHT_REQUESTS: usize = usize::MAX >> 3;

impl CustomProviderRequestPolicyConfig {
    pub(super) fn validate(&self, provider_name: &str) -> Result<(), String> {
        if self.max_in_flight_requests == Some(0) {
            return Err(format!(
                "custom_providers[{provider_name}].request_policy: `max_in_flight_requests` must be greater than 0"
            ));
        }
        if self.max_in_flight_requests.is_some_and(|limit| limit > MAX_IN_FLIGHT_REQUESTS) {
            return Err(format!(
                "custom_providers[{provider_name}].request_policy: `max_in_flight_requests` must not exceed {MAX_IN_FLIGHT_REQUESTS}"
            ));
        }
        if self.queue_timeout_seconds == 0 {
            return Err(format!(
                "custom_providers[{provider_name}].request_policy: `queue_timeout_seconds` must be greater than 0"
            ));
        }
        if self.retry_initial_backoff_ms == 0 {
            return Err(format!(
                "custom_providers[{provider_name}].request_policy: `retry_initial_backoff_ms` must be greater than 0"
            ));
        }
        if self.retry_max_backoff_ms < self.retry_initial_backoff_ms {
            return Err(format!(
                "custom_providers[{provider_name}].request_policy: `retry_max_backoff_ms` must be greater than or equal to `retry_initial_backoff_ms`"
            ));
        }
        Ok(())
    }
}

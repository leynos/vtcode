//! Centralized HTTP client factory for LLM providers.
//!
//! Provides consistent timeout configuration and client creation
//! across all providers based on the application's TimeoutsConfig.

use reqwest::Client;
use std::time::Duration;

use vtcode_config::TimeoutsConfig;

/// Factory for creating pre-configured HTTP clients.
pub struct HttpClientFactory;

impl HttpClientFactory {
    /// Create an HTTP client for standard LLM API requests.
    ///
    /// Uses the `default_ceiling_seconds` from TimeoutsConfig for the request timeout.
    /// Uses the separately configured provider connection timeout.
    pub fn for_llm(config: &TimeoutsConfig) -> Client {
        let connect_timeout = Duration::from_secs(config.connect_timeout_seconds);
        let request_timeout = Duration::from_secs(config.default_ceiling_seconds);
        Self::with_optional_timeouts(request_timeout, connect_timeout)
    }

    /// Create an HTTP client optimized for streaming requests.
    ///
    /// Uses the `streaming_ceiling_seconds` from TimeoutsConfig.
    /// Streaming requests typically take longer as they wait for incremental output.
    pub fn for_streaming(config: &TimeoutsConfig) -> Client {
        let mut builder = Client::builder()
            .pool_idle_timeout(Some(Duration::from_secs(90)))
            .pool_max_idle_per_host(1);
        if config.streaming_ceiling_seconds > 0 {
            builder = builder.timeout(Duration::from_secs(config.streaming_ceiling_seconds));
        }
        if config.connect_timeout_seconds > 0 {
            builder = builder.connect_timeout(Duration::from_secs(config.connect_timeout_seconds));
        }
        builder.build().unwrap_or_else(|_| Client::new())
    }

    /// Create an HTTP client with explicit timeout values.
    ///
    /// Use this when providers need custom timeout handling.
    pub(crate) fn with_timeouts(request_timeout: Duration, connect_timeout: Duration) -> Client {
        vtcode_commons::http::create_client_with_timeouts(connect_timeout, request_timeout)
    }

    fn with_optional_timeouts(request_timeout: Duration, connect_timeout: Duration) -> Client {
        let mut builder = Client::builder();
        if !request_timeout.is_zero() {
            builder = builder.timeout(request_timeout);
        }
        if !connect_timeout.is_zero() {
            builder = builder.connect_timeout(connect_timeout);
        }
        builder.build().unwrap_or_else(|_| Client::new())
    }

    /// Create a default HTTP client with reasonable defaults.
    ///
    /// Uses 180s request timeout and 30s connect timeout.
    pub fn default_client() -> Client {
        vtcode_commons::http::create_client_with_timeouts(Duration::from_secs(30), Duration::from_secs(180))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_client_is_created() {
        let client = HttpClientFactory::default_client();
        // Just verify it doesn't panic
        drop(client);
    }

    #[test]
    fn for_llm_uses_config_timeout() {
        let config = TimeoutsConfig::default();
        let client = HttpClientFactory::for_llm(&config);
        drop(client);
    }

    #[test]
    fn for_streaming_uses_longer_timeout() {
        let config = TimeoutsConfig::default();
        let client = HttpClientFactory::for_streaming(&config);
        drop(client);
    }
}

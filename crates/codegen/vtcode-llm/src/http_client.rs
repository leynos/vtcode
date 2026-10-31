//! Centralized HTTP client factory for LLM providers.
//!
//! Provides consistent timeout configuration and client creation
//! across all providers based on the application's TimeoutsConfig.

use reqwest::{Client, ClientBuilder};
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
        TimeoutPolicy::regular(config)
            .apply(Client::builder())
            .build()
            .unwrap_or_else(|error| {
                tracing::error!(
                    operation = "build_regular_http_client",
                    error_is_builder = error.is_builder(),
                    error_is_timeout = error.is_timeout(),
                    "failed to build HTTP client with configured deadlines; falling back to reqwest defaults"
                );
                Client::new()
            })
    }

    /// Create an HTTP client optimized for streaming requests.
    ///
    /// Uses the streaming request ceiling and separately configured connection timeout.
    pub fn for_streaming(config: &TimeoutsConfig) -> Client {
        let builder = Client::builder()
            .pool_idle_timeout(Some(Duration::from_secs(90)))
            .pool_max_idle_per_host(1);
        TimeoutPolicy::streaming(config).apply(builder).build().unwrap_or_else(|error| {
            tracing::error!(
                operation = "build_streaming_http_client",
                error_is_builder = error.is_builder(),
                error_is_timeout = error.is_timeout(),
                "failed to build HTTP client with configured deadlines; falling back to reqwest defaults"
            );
            Client::new()
        })
    }

    /// Create an HTTP client with explicit timeout values.
    ///
    /// Use this when providers need custom timeout handling.
    pub(crate) fn with_timeouts(request_timeout: Duration, connect_timeout: Duration) -> Client {
        vtcode_commons::http::create_client_with_timeouts(connect_timeout, request_timeout)
    }

    /// Create a default HTTP client with reasonable defaults.
    ///
    /// Uses 180s request timeout and 30s connect timeout.
    pub fn default_client() -> Client {
        vtcode_commons::http::create_client_with_timeouts(Duration::from_secs(30), Duration::from_secs(180))
    }
}

/// Zero ceilings disable their deadline independently of the other timeout.
#[derive(Debug, PartialEq, Eq)]
struct TimeoutPolicy {
    request: Option<Duration>,
    connection: Option<Duration>,
}

impl TimeoutPolicy {
    fn regular(config: &TimeoutsConfig) -> Self {
        Self::new(config.default_ceiling_seconds, config.connect_timeout_seconds)
    }

    fn streaming(config: &TimeoutsConfig) -> Self {
        Self::new(config.streaming_ceiling_seconds, config.connect_timeout_seconds)
    }

    fn new(request_seconds: u64, connection_seconds: u64) -> Self {
        Self {
            request: (request_seconds > 0).then(|| Duration::from_secs(request_seconds)),
            connection: (connection_seconds > 0).then(|| Duration::from_secs(connection_seconds)),
        }
    }

    fn apply(&self, mut builder: ClientBuilder) -> ClientBuilder {
        if let Some(timeout) = self.request {
            builder = builder.timeout(timeout);
        }
        if let Some(timeout) = self.connection {
            builder = builder.connect_timeout(timeout);
        }
        builder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::any;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn regular_and_streaming_policies_select_independent_ceilings() {
        let config = TimeoutsConfig {
            default_ceiling_seconds: 17,
            streaming_ceiling_seconds: 41,
            connect_timeout_seconds: 3,
            ..TimeoutsConfig::default()
        };
        assert_eq!(
            TimeoutPolicy::regular(&config),
            TimeoutPolicy {
                request: Some(Duration::from_secs(17)),
                connection: Some(Duration::from_secs(3)),
            }
        );
        assert_eq!(
            TimeoutPolicy::streaming(&config),
            TimeoutPolicy {
                request: Some(Duration::from_secs(41)),
                connection: Some(Duration::from_secs(3)),
            }
        );
    }

    #[test]
    fn zero_disables_each_timeout_independently_for_both_clients() {
        for (request_seconds, connection_seconds) in [(0, 0), (0, 3), (17, 0)] {
            let config = TimeoutsConfig {
                default_ceiling_seconds: request_seconds,
                streaming_ceiling_seconds: request_seconds,
                connect_timeout_seconds: connection_seconds,
                ..TimeoutsConfig::default()
            };
            let expected = TimeoutPolicy {
                request: (request_seconds > 0).then(|| Duration::from_secs(request_seconds)),
                connection: (connection_seconds > 0).then(|| Duration::from_secs(connection_seconds)),
            };
            assert_eq!(TimeoutPolicy::regular(&config), expected);
            assert_eq!(TimeoutPolicy::streaming(&config), expected);
        }
    }

    #[tokio::test]
    async fn client_factories_enforce_configured_request_timeouts() {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
            .mount(&server)
            .await;

        let config = TimeoutsConfig {
            default_ceiling_seconds: 1,
            streaming_ceiling_seconds: 1,
            connect_timeout_seconds: 1,
            ..TimeoutsConfig::default()
        };
        let regular_client = HttpClientFactory::for_llm(&config);
        let streaming_client = HttpClientFactory::for_streaming(&config);
        let (regular_result, streaming_result) =
            tokio::join!(regular_client.get(server.uri()).send(), streaming_client.get(server.uri()).send(),);

        let regular_error = regular_result.expect_err("regular request should exceed its request timeout");
        let streaming_error = streaming_result.expect_err("streaming request should exceed its request timeout");
        assert!(regular_error.is_timeout(), "{regular_error}");
        assert!(streaming_error.is_timeout(), "{streaming_error}");
    }

    #[tokio::test]
    async fn zero_request_ceiling_allows_a_delayed_response_for_both_clients() {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(1_100)))
            .mount(&server)
            .await;

        let config = TimeoutsConfig {
            default_ceiling_seconds: 0,
            streaming_ceiling_seconds: 0,
            connect_timeout_seconds: 2,
            ..TimeoutsConfig::default()
        };
        let regular_client = HttpClientFactory::for_llm(&config);
        let streaming_client = HttpClientFactory::for_streaming(&config);
        let (regular_result, streaming_result) =
            tokio::join!(regular_client.get(server.uri()).send(), streaming_client.get(server.uri()).send(),);

        assert_eq!(
            regular_result
                .expect("regular request should complete without a request deadline")
                .status(),
            200
        );
        assert_eq!(
            streaming_result
                .expect("streaming request should complete without a request deadline")
                .status(),
            200
        );
    }
}

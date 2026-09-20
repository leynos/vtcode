//! Provider-specific mappings from response headers to rate-limit metadata.

use serde::{Deserialize, Serialize};

macro_rules! rate_limit_header_fields {
    ($callback:ident) => {
        $callback!(
            requests_limit_per_minute => Some("x-ratelimit-limit-requests".to_string()),
            requests_remaining_per_minute => Some("x-ratelimit-remaining-requests".to_string()),
            tokens_limit_per_minute => Some("x-ratelimit-limit-tokens".to_string()),
            tokens_remaining_per_minute => Some("x-ratelimit-remaining-tokens".to_string()),
            requests_limit_per_second => None,
            requests_remaining_per_second => None,
            tokens_limit_per_second => None,
            tokens_remaining_per_second => None,
            prompt_tokens_limit_per_second => None,
            cache_adjusted_prompt_tokens_limit_per_second => None,
            generated_tokens_limit_per_second => None,
            prompt_tokens => None,
            cached_prompt_tokens => None,
            #[doc = "Header containing a provider-suggested reset interval in seconds."]
            reset_after_seconds => None,
        );
    };
}

macro_rules! define_rate_limit_header_config {
    ($( $(#[$field_attr:meta])* $field:ident => $default:expr ),+ $(,)?) => {
        /// Semantic mapping from provider response metadata to rate-limit headers.
        ///
        /// Header names are configurable because OpenAI-compatible providers expose
        /// equivalent quota information under different names. The default mapping
        /// covers the four Baseten/OpenAI-style per-minute headers.
        #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
        #[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
        pub struct RateLimitHeaderConfig {
            $(
                $(#[$field_attr])*
                #[serde(default, skip_serializing_if = "Option::is_none")]
                pub $field: Option<String>,
            )+
        }

        impl Default for RateLimitHeaderConfig {
            fn default() -> Self {
                Self {
                    $($field: $default,)+
                }
            }
        }

        impl RateLimitHeaderConfig {
            pub(super) fn fill_missing_from(&mut self, defaults: &Self) {
                $(
                    if self.$field.is_none() {
                        self.$field.clone_from(&defaults.$field);
                    }
                )+
            }

            pub(super) fn validate(&self, provider_name: &str) -> Result<(), String> {
                let headers = [
                    $((stringify!($field), &self.$field),)+
                ];

                for (field, header) in headers {
                    if header.as_deref().is_some_and(|header| !is_valid_header_name(header)) {
                        return Err(format!(
                            "custom_providers[{provider_name}].rate_limit_headers: `{field}` must be a valid HTTP header name"
                        ));
                    }
                }
                Ok(())
            }
        }
    };
}

rate_limit_header_fields!(define_rate_limit_header_config);

impl RateLimitHeaderConfig {
    pub(super) fn is_default(&self) -> bool {
        self == &Self::default()
    }

    fn fireworks_defaults() -> Self {
        Self {
            prompt_tokens_limit_per_second: Some("x-ratelimit-limit-tokens-prompt".to_string()),
            cache_adjusted_prompt_tokens_limit_per_second: Some(
                "x-ratelimit-limit-tokens-cache-adjusted-prompt".to_string(),
            ),
            generated_tokens_limit_per_second: Some("x-ratelimit-limit-tokens-generated".to_string()),
            prompt_tokens: Some("fireworks-prompt-tokens".to_string()),
            cached_prompt_tokens: Some("fireworks-cached-prompt-tokens".to_string()),
            ..Self::default()
        }
    }

    fn together_defaults() -> Self {
        Self {
            requests_limit_per_second: Some("x-ratelimit-limit".to_string()),
            requests_remaining_per_second: Some("x-ratelimit-remaining".to_string()),
            tokens_limit_per_second: Some("x-tokenlimit-limit".to_string()),
            tokens_remaining_per_second: Some("x-tokenlimit-remaining".to_string()),
            reset_after_seconds: Some("x-ratelimit-reset".to_string()),
            ..Self::default()
        }
    }

    fn anthropic_defaults() -> Self {
        Self {
            requests_limit_per_minute: Some("anthropic-ratelimit-requests-limit".to_string()),
            requests_remaining_per_minute: Some("anthropic-ratelimit-requests-remaining".to_string()),
            tokens_limit_per_minute: Some("anthropic-ratelimit-tokens-limit".to_string()),
            tokens_remaining_per_minute: Some("anthropic-ratelimit-tokens-remaining".to_string()),
            ..Self::default()
        }
    }

    /// Return default mappings for a provider name.
    ///
    /// The four per-minute mappings are universal custom-provider defaults;
    /// recognized Anthropic, Fireworks, and Together aliases add their
    /// documented fields.
    pub fn for_provider_name(provider_name: &str) -> Self {
        let normalized_name = provider_name.to_ascii_lowercase();
        if normalized_name.contains("anthropic") {
            Self::anthropic_defaults()
        } else if normalized_name.contains("fireworks") {
            Self::fireworks_defaults()
        } else if normalized_name.contains("together") {
            Self::together_defaults()
        } else {
            Self::default()
        }
    }
}

fn is_valid_header_name(header: &str) -> bool {
    !header.is_empty()
        && header.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

#[cfg(test)]
mod tests {
    use super::RateLimitHeaderConfig;

    #[test]
    fn rate_limit_header_defaults_keep_the_serialized_baseten_shape() {
        let serialized = serde_json::to_value(RateLimitHeaderConfig::default())
            .expect("rate-limit header defaults should serialize");

        assert_eq!(
            serialized,
            serde_json::json!({
                "requests_limit_per_minute": "x-ratelimit-limit-requests",
                "requests_remaining_per_minute": "x-ratelimit-remaining-requests",
                "tokens_limit_per_minute": "x-ratelimit-limit-tokens",
                "tokens_remaining_per_minute": "x-ratelimit-remaining-tokens",
            })
        );
    }

    #[test]
    fn provider_default_header_mappings_are_selected_case_insensitively() {
        let cases = [
            ("BASEten-glm", RateLimitHeaderConfig::default()),
            (
                "FireWorks-private",
                RateLimitHeaderConfig {
                    prompt_tokens_limit_per_second: Some("x-ratelimit-limit-tokens-prompt".to_string()),
                    cache_adjusted_prompt_tokens_limit_per_second: Some(
                        "x-ratelimit-limit-tokens-cache-adjusted-prompt".to_string(),
                    ),
                    generated_tokens_limit_per_second: Some("x-ratelimit-limit-tokens-generated".to_string()),
                    prompt_tokens: Some("fireworks-prompt-tokens".to_string()),
                    cached_prompt_tokens: Some("fireworks-cached-prompt-tokens".to_string()),
                    ..RateLimitHeaderConfig::default()
                },
            ),
            (
                "tOgEtHeR-router",
                RateLimitHeaderConfig {
                    requests_limit_per_second: Some("x-ratelimit-limit".to_string()),
                    requests_remaining_per_second: Some("x-ratelimit-remaining".to_string()),
                    tokens_limit_per_second: Some("x-tokenlimit-limit".to_string()),
                    tokens_remaining_per_second: Some("x-tokenlimit-remaining".to_string()),
                    reset_after_seconds: Some("x-ratelimit-reset".to_string()),
                    ..RateLimitHeaderConfig::default()
                },
            ),
            (
                "ANTHROPIC",
                RateLimitHeaderConfig {
                    requests_limit_per_minute: Some("anthropic-ratelimit-requests-limit".to_string()),
                    requests_remaining_per_minute: Some("anthropic-ratelimit-requests-remaining".to_string()),
                    tokens_limit_per_minute: Some("anthropic-ratelimit-tokens-limit".to_string()),
                    tokens_remaining_per_minute: Some("anthropic-ratelimit-tokens-remaining".to_string()),
                    ..RateLimitHeaderConfig::default()
                },
            ),
        ];

        for (provider_name, expected) in cases {
            assert_eq!(RateLimitHeaderConfig::for_provider_name(provider_name), expected, "{provider_name}");
        }
    }

    #[test]
    fn anthropic_defaults_map_numeric_headers_without_reset_timestamp() {
        let headers = RateLimitHeaderConfig::for_provider_name("Anthropic");

        assert_eq!(headers.requests_limit_per_minute.as_deref(), Some("anthropic-ratelimit-requests-limit"));
        assert_eq!(headers.requests_remaining_per_minute.as_deref(), Some("anthropic-ratelimit-requests-remaining"));
        assert_eq!(headers.tokens_limit_per_minute.as_deref(), Some("anthropic-ratelimit-tokens-limit"));
        assert_eq!(headers.tokens_remaining_per_minute.as_deref(), Some("anthropic-ratelimit-tokens-remaining"));
        assert_eq!(headers.reset_after_seconds, None);
    }

    #[test]
    fn rate_limit_header_validation_rejects_invalid_names() {
        let headers = RateLimitHeaderConfig {
            prompt_tokens: Some("not a header".to_string()),
            ..RateLimitHeaderConfig::default()
        };

        assert!(headers.validate("mycorp").is_err_and(|error| error.contains("prompt_tokens")));
    }
}

use vtcode_commons::ErrorCategory;
use vtcode_core::llm::provider::{LLMError, LLMErrorMetadata};

/// The bounded provider failure detail that ACP may expose to a client or
/// structured tracing. Provider diagnostics remain inside the provider layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SafeProviderProjection {
    category: &'static str,
    status: Option<u16>,
}

impl SafeProviderProjection {
    pub(super) fn from_llm_error(error: &LLMError) -> Self {
        Self {
            category: ErrorCategory::from(error).as_str(),
            status: error_metadata(error).and_then(|metadata| metadata.status),
        }
    }

    pub(super) const fn execution_error() -> Self {
        Self {
            category: ErrorCategory::ExecutionError.as_str(),
            status: None,
        }
    }

    pub(super) const fn category(self) -> &'static str {
        self.category
    }

    pub(super) const fn status(self) -> Option<u16> {
        self.status
    }

    pub(super) fn client_message(self) -> String {
        match self.status {
            Some(status) => format!("Provider failure category: {} (HTTP status {status}).", self.category),
            None => format!("Provider failure category: {}.", self.category),
        }
    }
}

fn error_metadata(error: &LLMError) -> Option<&LLMErrorMetadata> {
    match error {
        LLMError::Authentication { metadata, .. }
        | LLMError::RateLimit { metadata }
        | LLMError::InvalidRequest { metadata, .. }
        | LLMError::Network { metadata, .. }
        | LLMError::Provider { metadata, .. } => metadata.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use super::SafeProviderProjection;
    use vtcode_core::llm::provider::{LLMError, LLMErrorMetadata};

    #[test]
    fn safe_projection_excludes_provider_diagnostics() {
        let marker = "provider-marker://model/request/session/error-code/body";
        let error = LLMError::Provider {
            message: marker.to_string(),
            metadata: Some(LLMErrorMetadata::new(
                marker,
                Some(429),
                Some(marker.to_string()),
                Some(marker.to_string()),
                Some(marker.to_string()),
                None,
                Some(marker.to_string()),
            )),
        };

        let projection = SafeProviderProjection::from_llm_error(&error);

        assert_eq!(projection.category(), "Rate limit exceeded");
        assert_eq!(projection.status(), Some(429));
        assert!(!projection.client_message().contains(marker));
    }
}

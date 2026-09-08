//! Sanitized LLM error metadata and error wire types.

use serde::{Deserialize, Serialize, ser::SerializeStruct};
use std::fmt;

use crate::sanitizer::sanitize_provider_diagnostic;

/// Input details attached to an LLM provider error.
///
/// This structure groups provider response identity, retry guidance, and the
/// raw diagnostic before [`LLMErrorMetadata::new`] sanitizes the diagnostic.
#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct LLMErrorMetadata {
    provider: Option<String>,
    pub status: Option<u16>,
    pub code: Option<String>,
    request_id: Option<String>,
    organization_id: Option<String>,
    pub retry_after: Option<String>,
    pub message: Option<String>,
}

impl fmt::Debug for LLMErrorMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LLMErrorMetadata")
            .field("provider", &self.provider)
            .field("status", &self.status)
            .field("code", &self.code)
            .field("request_id", &self.request_id)
            .field("organization_id", &self.organization_id)
            .field("retry_after", &self.retry_after)
            .field(
                "message",
                &self
                    .message
                    .as_deref()
                    .map(|message| sanitize_provider_diagnostic(message.as_bytes())),
            )
            .finish()
    }
}

impl Serialize for LLMErrorMetadata {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("LLMErrorMetadata", 7)?;
        state.serialize_field("provider", &self.provider)?;
        state.serialize_field("status", &self.status)?;
        state.serialize_field("code", &self.code)?;
        state.serialize_field("request_id", &self.request_id)?;
        state.serialize_field("organization_id", &self.organization_id)?;
        state.serialize_field("retry_after", &self.retry_after)?;
        let message = self
            .message
            .as_deref()
            .map(|message| sanitize_provider_diagnostic(message.as_bytes()));
        state.serialize_field("message", &message)?;
        state.end()
    }
}

impl LLMErrorMetadata {
    /// Boxed constructor because metadata is always stored inside `Option<Box<LLMErrorMetadata>>`
    /// in the LLMError enum variants.
    #[must_use]
    pub fn new(
        provider: impl Into<String>,
        status: Option<u16>,
        code: Option<String>,
        request_id: Option<String>,
        organization_id: Option<String>,
        retry_after: Option<String>,
        message: Option<String>,
    ) -> Box<Self> {
        Box::new(Self {
            provider: Some(provider.into()),
            status,
            code,
            request_id,
            organization_id,
            retry_after,
            message: message.map(|message| sanitize_provider_diagnostic(message.as_bytes())),
        })
    }
}

/// LLM error types with optional provider metadata.
#[derive(Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LLMError {
    Authentication {
        message: String,
        metadata: Option<Box<LLMErrorMetadata>>,
    },
    RateLimit {
        metadata: Option<Box<LLMErrorMetadata>>,
    },
    InvalidRequest {
        message: String,
        metadata: Option<Box<LLMErrorMetadata>>,
    },
    Network {
        message: String,
        metadata: Option<Box<LLMErrorMetadata>>,
    },
    Provider {
        message: String,
        metadata: Option<Box<LLMErrorMetadata>>,
    },
}

impl fmt::Debug for LLMError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authentication { message, metadata } => formatter
                .debug_struct("Authentication")
                .field("message", &sanitize_provider_diagnostic(message.as_bytes()))
                .field("metadata", metadata)
                .finish(),
            Self::RateLimit { metadata } => formatter.debug_struct("RateLimit").field("metadata", metadata).finish(),
            Self::InvalidRequest { message, metadata } => formatter
                .debug_struct("InvalidRequest")
                .field("message", &sanitize_provider_diagnostic(message.as_bytes()))
                .field("metadata", metadata)
                .finish(),
            Self::Network { message, metadata } => formatter
                .debug_struct("Network")
                .field("message", &sanitize_provider_diagnostic(message.as_bytes()))
                .field("metadata", metadata)
                .finish(),
            Self::Provider { message, metadata } => formatter
                .debug_struct("Provider")
                .field("message", &sanitize_provider_diagnostic(message.as_bytes()))
                .field("metadata", metadata)
                .finish(),
        }
    }
}

impl fmt::Display for LLMError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authentication { message, .. } => {
                write!(formatter, "Authentication failed: {}", sanitize_provider_diagnostic(message.as_bytes()))
            }
            Self::RateLimit { .. } => formatter.write_str("Rate limit exceeded"),
            Self::InvalidRequest { message, .. } => {
                write!(formatter, "Invalid request: {}", sanitize_provider_diagnostic(message.as_bytes()))
            }
            Self::Network { message, .. } => {
                write!(formatter, "Network error: {}", sanitize_provider_diagnostic(message.as_bytes()))
            }
            Self::Provider { message, .. } => {
                write!(formatter, "Provider error: {}", sanitize_provider_diagnostic(message.as_bytes()))
            }
        }
    }
}

impl std::error::Error for LLMError {}

impl Serialize for LLMError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Authentication { message, metadata } => {
                let mut state = serializer.serialize_struct("LLMError", 3)?;
                state.serialize_field("type", "authentication")?;
                state.serialize_field("message", &sanitize_provider_diagnostic(message.as_bytes()))?;
                state.serialize_field("metadata", metadata)?;
                state.end()
            }
            Self::RateLimit { metadata } => {
                let mut state = serializer.serialize_struct("LLMError", 2)?;
                state.serialize_field("type", "rate_limit")?;
                state.serialize_field("metadata", metadata)?;
                state.end()
            }
            Self::InvalidRequest { message, metadata } => {
                let mut state = serializer.serialize_struct("LLMError", 3)?;
                state.serialize_field("type", "invalid_request")?;
                state.serialize_field("message", &sanitize_provider_diagnostic(message.as_bytes()))?;
                state.serialize_field("metadata", metadata)?;
                state.end()
            }
            Self::Network { message, metadata } => {
                let mut state = serializer.serialize_struct("LLMError", 3)?;
                state.serialize_field("type", "network")?;
                state.serialize_field("message", &sanitize_provider_diagnostic(message.as_bytes()))?;
                state.serialize_field("metadata", metadata)?;
                state.end()
            }
            Self::Provider { message, metadata } => {
                let mut state = serializer.serialize_struct("LLMError", 3)?;
                state.serialize_field("type", "provider")?;
                state.serialize_field("message", &sanitize_provider_diagnostic(message.as_bytes()))?;
                state.serialize_field("metadata", metadata)?;
                state.end()
            }
        }
    }
}

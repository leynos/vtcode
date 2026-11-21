//! Typed API format selection for custom providers.
use serde::{Deserialize, Serialize};

pub(super) fn skip_serializing_custom_provider_api_format(api_format: &CustomProviderApiFormat) -> bool {
    api_format.is_auto()
}

/// Typed API format used by custom providers.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum CustomProviderApiFormat {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "openai-chat")]
    OpenAIChat,
    #[serde(rename = "openai-responses")]
    OpenAIResponses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
}

impl CustomProviderApiFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::OpenAIChat => "openai-chat",
            Self::OpenAIResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }

    pub const fn is_auto(self) -> bool {
        matches!(self, Self::Auto)
    }

    pub const fn resolved(self) -> Option<Self> {
        match self {
            Self::Auto => None,
            other => Some(other),
        }
    }
}

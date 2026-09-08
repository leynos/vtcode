//! Typed conversion from LLM errors into the shared taxonomy.

use std::fmt::Write;

use super::{ErrorCategory, classify_error_message};

impl From<&crate::llm::LLMError> for ErrorCategory {
    fn from(err: &crate::llm::LLMError) -> Self {
        match err {
            crate::llm::LLMError::Authentication { .. } => ErrorCategory::Authentication,
            crate::llm::LLMError::RateLimit { metadata } => {
                classify_llm_metadata(metadata.as_deref(), ErrorCategory::RateLimit)
            }
            crate::llm::LLMError::InvalidRequest { .. } => ErrorCategory::InvalidParameters,
            crate::llm::LLMError::Network { .. } => ErrorCategory::Network,
            crate::llm::LLMError::Provider { message, metadata } => {
                let metadata_category = classify_llm_metadata(metadata.as_deref(), ErrorCategory::ExecutionError);
                if metadata_category != ErrorCategory::ExecutionError {
                    return metadata_category;
                }

                // Check metadata status code first for precise classification
                if let Some(meta) = metadata
                    && let Some(status) = meta.status
                {
                    return match status {
                        401 => ErrorCategory::Authentication,
                        403 => ErrorCategory::PermissionDenied,
                        404 => ErrorCategory::ResourceNotFound,
                        429 => ErrorCategory::RateLimit,
                        400 => ErrorCategory::InvalidParameters,
                        500 | 502 | 503 | 504 => ErrorCategory::ServiceUnavailable,
                        408 => ErrorCategory::Timeout,
                        _ => classify_error_message(message),
                    };
                }
                // Fall back to message-based classification
                classify_error_message(message)
            }
        }
    }
}

fn classify_llm_metadata(metadata: Option<&crate::llm::LLMErrorMetadata>, fallback: ErrorCategory) -> ErrorCategory {
    let Some(metadata) = metadata else {
        return fallback;
    };

    let mut hint = String::new();
    if let Some(code) = &metadata.code {
        hint.push_str(code);
        hint.push(' ');
    }
    if let Some(message) = &metadata.message {
        hint.push_str(message);
        hint.push(' ');
    }
    if let Some(status) = metadata.status {
        let _ = write!(&mut hint, "{status}");
    }

    let classified = classify_error_message(&hint);
    if classified == ErrorCategory::ExecutionError {
        fallback
    } else {
        classified
    }
}

#![expect(
    clippy::let_underscore_must_use,
    reason = "The category formatter intentionally ignores infallible formatting results."
)]

//! Canonical classification of operational errors and retry guidance.

mod category;
mod guidance;
mod llm;
mod message;
mod types;

pub use message::{
    classify_anyhow_error, classify_error_message, is_context_capacity_error, is_context_capacity_message,
    is_retryable_llm_error_message,
};
pub use types::{BackoffStrategy, ErrorCategory, Retryability};

#[cfg(test)]
mod tests;

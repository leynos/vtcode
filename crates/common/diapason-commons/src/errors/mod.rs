#![expect(
    clippy::indexing_slicing,
    reason = "Error rendering indexes only validated structured diagnostic entries."
)]

//! Error reporting primitives, standard error text, and error aggregation.

mod constants;
mod multi_errors;
mod reporting;

pub use constants::*;
pub use multi_errors::MultiErrors;
pub use reporting::{DisplayErrorFormatter, ErrorFormatter, ErrorReporter, NoopErrorReporter};

/// Format a file-operation error with its path.
#[macro_export]
macro_rules! file_err {
    ($path:expr, read) => {
        format!("failed to read {}", $path)
    };
    ($path:expr, write) => {
        format!("failed to write {}", $path)
    };
    ($path:expr, delete) => {
        format!("failed to delete {}", $path)
    };
    ($path:expr, create) => {
        format!("failed to create {}", $path)
    };
}

/// Helper macro for context errors
/// Usage: ctx_err!(operation, context) -> "operation context"
#[macro_export]
macro_rules! ctx_err {
    ($op:expr, $ctx:expr) => {
        format!("{}: {}", $op, $ctx)
    };
}

#[cfg(test)]
mod tests;

//! Emit bounded audit I/O diagnostics without disclosing sink paths.

pub(super) fn log_io_failure(operation: &'static str, error: &std::io::Error) {
    tracing::warn!(
        operation,
        error_kind = ?error.kind(),
        outcome = "failed",
        "JsonlFileSink: I/O operation failed"
    );
}

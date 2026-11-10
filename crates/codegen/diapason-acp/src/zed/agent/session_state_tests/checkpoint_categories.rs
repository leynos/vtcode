//! Compile-time coverage for bounded checkpoint categories.
//!
//! The category labels reach logs and client errors, so a new failure mode must
//! acquire a stable label at compile time rather than fail at runtime.

use crate::zed::agent::session_state::{CheckpointFailure, RequiredCheckpointError};

/// Compares two strings in a constant context.
///
/// `str` equality is not yet usable in constants, so labels are compared byte
/// by byte. This keeps the label check entirely in the compiler.
const fn labels_match(actual: &str, expected: &str) -> bool {
    let actual = actual.as_bytes();
    let expected = expected.as_bytes();
    if actual.len() != expected.len() {
        return false;
    }
    let mut index = 0;
    while index < actual.len() {
        if actual[index] != expected[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// Every failure category has the stable label used in logs and client errors.
///
/// `CheckpointFailure::label` is a `const fn`, so evaluating each variant in a
/// constant expression proves the label is available without a runtime call.
const _: () = {
    assert!(labels_match(CheckpointFailure::PersistenceDisabled.label(), "persistence_disabled"));
    assert!(labels_match(CheckpointFailure::WriteFailed.label(), "archive_write_failed"));
    assert!(labels_match(CheckpointFailure::ReadBackFailed.label(), "archive_read_back_failed"));
    assert!(labels_match(CheckpointFailure::RetentionDropped.label(), "staged_batch_dropped"));
};

/// Reads the category of a required-checkpoint error in a constant context.
///
/// A `RequiredCheckpointError` cannot be built in a constant because its cause
/// is an `anyhow::Error`, but defining this `const fn` still makes the compiler
/// check that `failure()` and `label()` are const-callable. Callers can then
/// rely on the category without inspecting the raw cause.
const fn required_checkpoint_failure_label(error: &RequiredCheckpointError) -> &'static str {
    error.failure().label()
}

/// Wrapping a cause keeps its bounded category.
#[test]
fn required_checkpoint_error_reports_its_category() {
    let error =
        RequiredCheckpointError::new(CheckpointFailure::RetentionDropped, anyhow::anyhow!("staged batch dropped"));

    assert_eq!(required_checkpoint_failure_label(&error), "staged_batch_dropped");
    assert_eq!(error.failure(), CheckpointFailure::RetentionDropped);
    assert_eq!(error.to_string(), "staged batch dropped");
}

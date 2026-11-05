//! Session-local repeat bound for failing `apply_patch` calls.
//!
//! The registry bypasses its generic identical-call detector for
//! `apply_patch`, because that detector counts only successful records and
//! would otherwise refuse the no-op ladder's third diagnostic occurrence. The
//! no-op ladder, in turn, only observes calls that reach planning: decode,
//! parse, version-precondition, missing-file, and conflict rejections all
//! return before it runs. Without this guard a model could resubmit one failing
//! patch indefinitely and never see a repeat bound.
//!
//! The guard keeps one signature and a consecutive-failure count. Two identical
//! failures are permitted so the model can read the diagnostic twice; the third
//! identical call is refused without touching the workspace. A success, or a
//! materially different request, starts the ladder over, so the bound only ever
//! applies to one stuck request rather than to the tool as a whole.

use crate::tools::edited_file_monitor::FILE_CONFLICT_DETECTED_FIELD;
use serde_json::Value;

/// Identical failures a single request may produce before it is refused.
///
/// Mirrors the no-op ladder, which permits two occurrences and blocks the
/// third, and the generic detector's default limit of two repeats.
pub(super) const MAX_IDENTICAL_FAILURES: u8 = 2;

/// Consecutive-failure state for one session's `apply_patch` calls.
#[derive(Debug, Default)]
pub(super) struct PatchFailureGuard {
    last_signature: Option<String>,
    consecutive_failures: u8,
}

impl PatchFailureGuard {
    /// Return this request's ordinal when it must be refused.
    ///
    /// `occurrence` is the position of the refused call in the ladder, not the
    /// number of completed failures, so the model is told it is the third
    /// identical attempt.
    pub(super) fn blocked_occurrence(&self, signature: &str) -> Option<u8> {
        let same_request = self.last_signature.as_deref() == Some(signature);
        (same_request && self.consecutive_failures >= MAX_IDENTICAL_FAILURES)
            .then(|| self.consecutive_failures.saturating_add(1))
    }

    /// Record one completed attempt against the request's signature.
    ///
    /// A success clears the ladder. A failure with a different signature starts
    /// a fresh ladder at one, because a materially different request is a new
    /// diagnostic rather than a repeat.
    pub(super) fn record(&mut self, signature: &str, failed: bool) {
        if !failed {
            self.reset();
            return;
        }
        if self.last_signature.as_deref() == Some(signature) {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        } else {
            self.last_signature = Some(signature.to_string());
            self.consecutive_failures = 1;
        }
    }

    /// Forget the previous signature so the next request starts the ladder again.
    fn reset(&mut self) {
        self.last_signature = None;
        self.consecutive_failures = 0;
    }
}

/// Hash the request the caller sent, so identical repeats share a signature.
///
/// The encoding sorts object keys and length-prefixes every field, so key
/// ordering cannot change the signature and adjacent fields cannot alias. A
/// caller that alternates equivalent key orders still hits the same ladder.
/// Transport aliases such as base64-encoded input name a different request
/// here; a stuck loop resubmits one payload, so that distinction does not
/// weaken the bound.
pub(super) fn request_signature(args: &Value) -> String {
    let mut encoded = String::new();
    encode_value(&mut encoded, args);
    format!("sha256:{}", diapason_commons::utils::calculate_sha256(encoded.as_bytes()))
}

/// Classify one finished attempt as a failure for the repeat bound.
///
/// A structured rejection envelope and a propagated error are both failures the
/// model must correct. A file conflict is reported as a success envelope with
/// `conflict_detected`, so it needs its own check; otherwise a model could
/// repeat one conflicting patch without limit.
pub(super) fn attempt_failed(outcome: &anyhow::Result<Value>) -> bool {
    match outcome {
        Ok(value) => {
            super::execution_facade::structured_tool_output_error(value).is_some()
                || value.get(FILE_CONFLICT_DETECTED_FIELD).and_then(Value::as_bool) == Some(true)
        }
        Err(_) => true,
    }
}

/// Append one JSON value to the signature encoding with a type tag.
fn encode_value(encoded: &mut String, value: &Value) {
    match value {
        Value::Null => encoded.push('0'),
        Value::Bool(true) => encoded.push('t'),
        Value::Bool(false) => encoded.push('f'),
        Value::Number(number) => {
            encoded.push('n');
            push_field(encoded, &number.to_string());
        }
        Value::String(text) => {
            encoded.push('s');
            push_field(encoded, text);
        }
        Value::Array(items) => {
            encoded.push('[');
            for item in items {
                encode_value(encoded, item);
            }
            encoded.push(']');
        }
        Value::Object(fields) => {
            encoded.push('{');
            let mut keys = fields.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for key in keys {
                push_field(encoded, key);
                if let Some(field) = fields.get(key) {
                    encode_value(encoded, field);
                }
            }
            encoded.push('}');
        }
    }
}

/// Append one length-prefixed field to the signature encoding.
fn push_field(encoded: &mut String, value: &str) {
    encoded.push_str(&value.len().to_string());
    encoded.push(':');
    encoded.push_str(value);
}

#[cfg(test)]
mod tests;

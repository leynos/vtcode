//! Redacted and bounded rendering of ACP failure detail for operator logs.
//!
//! ACP clients receive closed-set categories. The originating error may still
//! carry a workspace path, archive name, transport URL, or provider payload, so
//! the operator-facing rendering collapses path-like tokens, masks known secret
//! shapes, and truncates the result to a fixed budget. A bare file name with no
//! path separator cannot be told apart from ordinary prose, so it is left to
//! the secret-shape masking rather than collapsed as a path.

use diapason_commons::sanitizer::redact_secrets;

/// Maximum rendered detail size in bytes.
const DETAIL_MAX_BYTES: usize = 512;

/// Maximum rendered size of a free-form log label, in bytes.
const LABEL_MAX_BYTES: usize = 96;

const TRUNCATION_MARKER: &str = "… [detail truncated]";

/// Render untrusted failure detail for an operator log.
///
/// Path-like and URL-like tokens become `<path>`, secret shapes recognized by
/// [`redact_secrets`] become `[REDACTED_SECRET]`, and the result is bounded to
/// [`DETAIL_MAX_BYTES`] on a UTF-8 boundary.
pub(super) fn redact_detail(detail: &str) -> String {
    let redacted = redact_secrets(redact_path_like_tokens(detail));
    truncate(redacted)
}

/// Render a configured identifier as a bounded, control-free log label.
///
/// Provider names and model identifiers are operator-facing context rather
/// than secrets, so they stay readable, but they come from configuration and
/// may be arbitrarily long or carry control characters. Bounding them keeps a
/// single field from flooding or forging a log line.
pub(super) fn bounded_label(label: &str) -> String {
    let cleaned: String = label.chars().filter(|character| !character.is_control()).collect();
    truncate_to(cleaned, LABEL_MAX_BYTES)
}

/// Width of a rendered session correlation key, in hexadecimal characters.
pub(super) const SESSION_CORRELATION_HEX_LENGTH: usize = 12;

/// Return a deterministic, bounded correlation key for a session id.
///
/// Logs and spans correlate repeated events without carrying the raw session
/// identifier. The digest is stable across processes and runs, which a
/// randomly keyed hasher would not be; it is an identifier, not a secrecy
/// boundary.
pub(super) fn session_correlation(session_id: &str) -> String {
    diapason_commons::utils::calculate_sha256(session_id.as_bytes())
        .chars()
        .take(SESSION_CORRELATION_HEX_LENGTH)
        .collect()
}

/// Report a compaction activity update that could not be serialized or sent.
///
/// A failed update never fails the compaction itself, but the raw transport or
/// serialization error may embed workspace or archive detail, so the warning
/// carries the fixed category and a redacted, bounded rendering.
pub(super) fn log_compaction_update_failure(
    error: &impl std::fmt::Display,
    session_correlation: &str,
    stage: &'static str,
) {
    tracing::warn!(
        session_correlation = %session_correlation,
        stage,
        operation = "context_compaction_update",
        error_category = "compaction_update_failed",
        detail = %redact_detail(&error.to_string()),
        "Failed to publish an ACP compaction activity update"
    );
}

/// Report a provider rate-limit notice that could not be delivered.
///
/// The transport error may embed provider payload or session detail, so the
/// warning carries fixed fields and a redacted, bounded rendering instead of
/// the raw error or the raw session identifier.
pub(super) fn log_notice_publish_failure(error: &impl std::fmt::Display, session_correlation: &str) {
    tracing::warn!(
        session_correlation = %session_correlation,
        stage = "publish_rate_limit_notice",
        operation = "provider_rate_limit_notice",
        error_category = "rate_limit_notice_publish_failed",
        detail = %redact_detail(&error.to_string()),
        "Failed to publish an ACP provider rate-limit notice"
    );
}

fn redact_path_like_tokens(detail: &str) -> String {
    let mut redacted = String::with_capacity(detail.len());
    let mut token = String::new();
    for character in detail.chars() {
        if character.is_whitespace() {
            flush_token(&mut redacted, &mut token);
            redacted.push(character);
        } else {
            token.push(character);
        }
    }
    flush_token(&mut redacted, &mut token);
    redacted
}

fn flush_token(redacted: &mut String, token: &mut String) {
    if token.is_empty() {
        return;
    }
    if is_path_like(token) {
        redacted.push_str("<path>");
    } else {
        redacted.push_str(token);
    }
    token.clear();
}

/// Report whether `token` carries a path separator.
///
/// Both separators count on their own: a relative Windows path such as
/// `..\secrets\archive.json` has no drive prefix but is still path-like. A
/// bare file name with no separator cannot be distinguished from ordinary
/// prose, so it is left for [`redact_secrets`] to mask if it looks secret.
fn is_path_like(token: &str) -> bool {
    token.contains('/') || token.contains('\\')
}

fn truncate(detail: String) -> String {
    truncate_to(detail, DETAIL_MAX_BYTES)
}

fn truncate_to(mut detail: String, max_bytes: usize) -> String {
    if detail.len() <= max_bytes {
        return detail;
    }
    let content_limit = max_bytes.saturating_sub(TRUNCATION_MARKER.len());
    let end = detail.floor_char_boundary(content_limit);
    detail.truncate(end);
    detail.push_str(TRUNCATION_MARKER);
    detail
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A `tracing` writer that collects formatted output for one test only.
    #[derive(Clone, Default)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut guard = self
                .0
                .lock()
                .map_err(|poisoned| std::io::Error::other(format!("capture poisoned: {poisoned}")))?;
            guard.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Run `log` under a private subscriber and return the captured output.
    ///
    /// The subscriber is installed with `with_default` so parallel tests keep
    /// their own dispatch and cannot contaminate the capture.
    fn capture(log: impl FnOnce()) -> String {
        let captured = CaptureWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(captured.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, log);
        String::from_utf8(captured.0.lock().expect("capture lock").clone()).expect("captured output is UTF-8")
    }

    /// Assert the compaction update warning carries fixed fields only.
    #[test]
    fn compaction_update_failure_logs_fixed_fields_and_redacted_detail() {
        let correlation = session_correlation("session-alpha");
        let output = capture(|| {
            log_compaction_update_failure(
                &std::io::Error::other("cannot open /secret/workspace/archive.json"),
                &correlation,
                "publish_started",
            );
        });

        assert!(output.contains("operation=\"context_compaction_update\""), "{output}");
        assert!(output.contains("error_category=\"compaction_update_failed\""), "{output}");
        assert!(output.contains("stage=\"publish_started\""), "{output}");
        assert!(output.contains(&format!("session_correlation={correlation}")), "{output}");
        assert!(output.contains("<path>"), "the path must be collapsed: {output}");
        assert!(!output.contains("/secret/workspace"), "the workspace path must not survive: {output}");
    }

    /// Assert the rate-limit warning omits the raw error and session id.
    #[test]
    fn notice_publish_failure_logs_fixed_fields_and_redacted_detail() {
        let correlation = session_correlation("raw-session-identifier");
        let output = capture(|| {
            log_notice_publish_failure(
                &std::io::Error::other("transport closed for /home/private/workspace"),
                &correlation,
            );
        });

        assert!(output.contains("operation=\"provider_rate_limit_notice\""), "{output}");
        assert!(output.contains("error_category=\"rate_limit_notice_publish_failed\""), "{output}");
        assert!(output.contains("stage=\"publish_rate_limit_notice\""), "{output}");
        assert!(output.contains(&format!("session_correlation={correlation}")), "{output}");
        assert!(!output.contains("raw-session-identifier"), "the raw session id must not survive: {output}");
        assert!(!output.contains("/home/private/workspace"), "the workspace path must not survive: {output}");
    }

    /// Assert configured labels are bounded and free of control characters.
    #[test]
    fn bounded_labels_drop_control_characters_and_bound_length() {
        assert_eq!(bounded_label("openai/gpt-5"), "openai/gpt-5");
        assert_eq!(bounded_label("model\nwith\rcontrols"), "modelwithcontrols");

        let bounded = bounded_label(&"m".repeat(LABEL_MAX_BYTES * 2));
        assert!(bounded.len() <= LABEL_MAX_BYTES, "label must stay bounded: {}", bounded.len());
        assert!(bounded.ends_with(TRUNCATION_MARKER), "truncation is marked: {bounded}");
    }

    /// Assert unix and windows paths never reach the log.
    #[test]
    fn redacts_path_like_tokens() {
        let detail = redact_detail("failed to read /secret/workspace/archive.json and C:\\Users\\me\\secret.txt");

        assert!(!detail.contains("/secret"), "unix path must be collapsed: {detail}");
        assert!(!detail.contains("secret.txt"), "windows path must be collapsed: {detail}");
        assert!(detail.contains("<path>"), "paths are replaced with a marker: {detail}");
        assert!(detail.contains("failed to read"), "surrounding prose is preserved: {detail}");
    }

    /// Assert a relative Windows path is collapsed without a drive prefix.
    #[test]
    fn redacts_relative_windows_paths() {
        let detail = redact_detail("failed to read ..\\secrets\\archive.json and sessions\\a1.json");

        assert!(!detail.contains("secrets"), "relative windows path must be collapsed: {detail}");
        assert!(!detail.contains("a1.json"), "relative windows path must be collapsed: {detail}");
        assert_eq!(detail.matches("<path>").count(), 2, "both tokens are paths: {detail}");
    }

    /// Assert URLs and secret shapes are redacted.
    #[test]
    fn redacts_urls_and_secrets() {
        let detail =
            redact_detail("POST https://api.example.invalid/v1/chat failed with sk-abcdefghijklmnopqrstuvwxyz");

        assert!(!detail.contains("api.example.invalid"), "transport URL must be collapsed: {detail}");
        assert!(!detail.contains("sk-abcdefghijklmnopqrstuvwxyz"), "secret must be masked: {detail}");
        assert!(detail.contains("[REDACTED_SECRET]"), "secret marker must remain: {detail}");
    }

    /// Assert short prose without paths is left intact for diagnosis.
    #[test]
    fn preserves_plain_detail() {
        assert_eq!(redact_detail("provider returned an empty completion"), "provider returned an empty completion");
    }

    /// Assert rendering is bounded and UTF-8 safe.
    #[test]
    fn bounds_long_detail_on_a_character_boundary() {
        let detail = redact_detail(&"é".repeat(DETAIL_MAX_BYTES));

        assert!(detail.len() <= DETAIL_MAX_BYTES, "detail must stay bounded: {}", detail.len());
        assert!(detail.ends_with(TRUNCATION_MARKER), "truncation is marked: {detail}");
    }

    /// Assert correlation keys are stable, distinct and never the raw id.
    #[test]
    fn session_correlation_is_stable_and_non_identifying() {
        let first = session_correlation("session-alpha");
        let again = session_correlation("session-alpha");
        let other = session_correlation("session-beta");

        assert_eq!(first, again, "the same session must correlate across events");
        assert_ne!(first, other, "distinct sessions must not collide");
        assert!(!first.contains("session"), "the raw id must not survive: {first}");
        assert_eq!(first.len(), SESSION_CORRELATION_HEX_LENGTH, "the key has a fixed width");
    }
}

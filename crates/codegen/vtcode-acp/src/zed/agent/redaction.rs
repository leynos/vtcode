//! Redacted and bounded rendering of ACP failure detail for operator logs.
//!
//! ACP clients receive closed-set categories. The originating error may still
//! carry a workspace path, archive name, transport URL, or provider payload, so
//! the operator-facing rendering collapses path-like tokens, masks known secret
//! shapes, and truncates the result to a fixed budget.

use vtcode_commons::sanitizer::redact_secrets;

/// Maximum rendered detail size in bytes.
const DETAIL_MAX_BYTES: usize = 512;
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

/// Width of a rendered session correlation key, in hexadecimal characters.
pub(super) const SESSION_CORRELATION_HEX_LENGTH: usize = 12;

/// Return a deterministic, bounded correlation key for a session id.
///
/// Logs and spans correlate repeated events without carrying the raw session
/// identifier. The digest is stable across processes and runs, which a
/// randomly keyed hasher would not be; it is an identifier, not a secrecy
/// boundary.
pub(super) fn session_correlation(session_id: &str) -> String {
    vtcode_commons::utils::calculate_sha256(session_id.as_bytes())
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

fn is_path_like(token: &str) -> bool {
    if token.contains('/') {
        return true;
    }
    let mut characters = token.chars();
    let _drive = characters.next();
    characters.next() == Some(':') && token.contains('\\')
}

fn truncate(mut detail: String) -> String {
    if detail.len() <= DETAIL_MAX_BYTES {
        return detail;
    }
    let content_limit = DETAIL_MAX_BYTES.saturating_sub(TRUNCATION_MARKER.len());
    let end = detail.floor_char_boundary(content_limit);
    detail.truncate(end);
    detail.push_str(TRUNCATION_MARKER);
    detail
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert unix and windows paths never reach the log.
    #[test]
    fn redacts_path_like_tokens() {
        let detail = redact_detail("failed to read /secret/workspace/archive.json and C:\\Users\\me\\secret.txt");

        assert!(!detail.contains("/secret"), "unix path must be collapsed: {detail}");
        assert!(!detail.contains("secret.txt"), "windows path must be collapsed: {detail}");
        assert!(detail.contains("<path>"), "paths are replaced with a marker: {detail}");
        assert!(detail.contains("failed to read"), "surrounding prose is preserved: {detail}");
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

//! Fixed-field, bounded rendering of ACP failure diagnostics for operator logs.
//!
//! ACP clients receive closed-set categories. The originating error may still
//! carry a workspace path, archive name, transport URL, provider payload, hook
//! text, secret, or personal data, so none of its rendering is written to a log
//! at any level. Operators instead get fixed fields - operation, stage,
//! category, elapsed time, and byte counts - plus bounded, non-reversible
//! correlation keys that let repeated identical failures be grouped without
//! retaining the text that produced them. Configured identities - provider
//! names and model identifiers - are treated the same way: their domain and
//! content are not ours to trust, so they are logged as bounded digests with
//! their byte length rather than verbatim.

/// Width of a rendered correlation key, in hexadecimal characters.
pub(super) const CORRELATION_HEX_LENGTH: usize = 12;

/// Return a deterministic, bounded correlation key for a session id.
///
/// Logs and spans correlate repeated events without carrying the raw session
/// identifier. The digest is stable across processes and runs, which a
/// randomly keyed hasher would not be; it is an identifier, not a secrecy
/// boundary.
pub(super) fn session_correlation(session_id: &str) -> String {
    bounded_digest(session_id)
}

/// Return a deterministic, bounded correlation key for untrusted detail.
///
/// Repeated identical failures can be grouped across sessions and restarts
/// without retaining the provider payload, hook output, path, URL, or personal
/// data that produced the text. Like [`session_correlation`], the digest is
/// stable and bounded, and it is not a secrecy boundary.
pub(super) fn detail_correlation(detail: &str) -> String {
    bounded_digest(detail)
}

/// Return a deterministic, bounded correlation key for a configured identity.
///
/// Provider names arrive through the provider trait, and model identifiers come
/// from configuration or a provider response, so neither is a value this module
/// controls. A digest keeps distinct providers and models distinguishable and
/// stable across records without retaining the identifier itself, and it cannot
/// carry control characters or unbounded text into the log.
pub(super) fn identifier_correlation(identifier: &str) -> String {
    bounded_digest(identifier)
}

/// Convert a measurement interval into whole elapsed milliseconds.
///
/// Both endpoints are supplied because the publication commands read an
/// injected clock rather than the ambient one, so the interval is what the
/// clock reported and not what the process observed. Span fields and metrics
/// report whole milliseconds, and a duration beyond `u64::MAX` milliseconds
/// saturates rather than wrapping.
pub(super) fn elapsed_millis(started_at: std::time::Instant, finished_at: std::time::Instant) -> u64 {
    u64::try_from(finished_at.saturating_duration_since(started_at).as_millis()).unwrap_or(u64::MAX)
}

fn bounded_digest(value: &str) -> String {
    diapason_commons::utils::calculate_sha256(value.as_bytes())
        .chars()
        .take(CORRELATION_HEX_LENGTH)
        .collect()
}

/// Report a compaction activity update that could not be serialized or sent.
///
/// A failed update never fails the compaction itself, but the transport or
/// serialization error may embed workspace, archive, or provider detail, so the
/// warning carries fixed fields and a bounded digest instead of any part of the
/// error's rendering.
pub(super) fn log_compaction_update_failure(
    error: &impl std::fmt::Display,
    session_correlation: &str,
    stage: &'static str,
) {
    let detail = error.to_string();
    tracing::warn!(
        session_correlation = %session_correlation,
        stage,
        operation = "context_compaction_update",
        error_category = "compaction_update_failed",
        error_bytes = detail.len(),
        detail_correlation = %detail_correlation(&detail),
        "Failed to publish an ACP compaction activity update"
    );
}

/// Report a provider rate-limit notice that could not be delivered.
///
/// The transport error may embed provider payload or session detail, so the
/// warning carries fixed fields and a bounded digest instead of the raw error
/// or the raw session identifier.
pub(super) fn log_notice_publish_failure(error: &impl std::fmt::Display, session_correlation: &str) {
    let detail = error.to_string();
    tracing::warn!(
        session_correlation = %session_correlation,
        stage = "publish_rate_limit_notice",
        operation = "provider_rate_limit_notice",
        error_category = "rate_limit_notice_publish_failed",
        error_bytes = detail.len(),
        detail_correlation = %detail_correlation(&detail),
        "Failed to publish an ACP provider rate-limit notice"
    );
}

#[cfg(test)]
mod tests {
    //! Covers the fixed-field records that keep operator-sensitive detail out
    //! of ACP failure logging.
    //!
    //! Each test installs its own capture subscriber, so these assertions pin
    //! the bounded fields a record keeps and the payload text it must never
    //! carry.

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

    /// Adversarial failure text that must never be log-visible.
    const ADVERSARIAL_DETAIL: &str = concat!(
        "provider payload POST https://api.example.invalid/v1/chat?token=SUPERSECRETTOKEN ",
        "hook /home/private/hooks/pre.sh output alice@example.com ",
        "sk-abcdefghijklmnopqrstuvwxyz archive /secret/workspace/archive.json ",
        "raw-session-identifier"
    );

    /// Assert no part of [`ADVERSARIAL_DETAIL`] survives in `output`.
    fn assert_detail_is_absent(output: &str) {
        for leaked in [
            "provider payload",
            "api.example.invalid",
            "SUPERSECRETTOKEN",
            "/home/private",
            "pre.sh",
            "alice@example.com",
            "sk-abcdefghijklmnopqrstuvwxyz",
            "/secret/workspace",
            "raw-session-identifier",
        ] {
            assert!(!output.contains(leaked), "{leaked} must not reach the log: {output}");
        }
    }

    /// Assert the compaction update warning carries fixed fields only.
    #[test]
    fn compaction_update_failure_logs_fixed_fields_only() {
        let correlation = session_correlation("session-alpha");
        let output = capture(|| {
            log_compaction_update_failure(&std::io::Error::other(ADVERSARIAL_DETAIL), &correlation, "publish_started");
        });

        assert!(output.contains("operation=\"context_compaction_update\""), "{output}");
        assert!(output.contains("error_category=\"compaction_update_failed\""), "{output}");
        assert!(output.contains("stage=\"publish_started\""), "{output}");
        assert!(output.contains(&format!("session_correlation={correlation}")), "{output}");
        assert!(output.contains(&format!("error_bytes={}", ADVERSARIAL_DETAIL.len())), "{output}");
        assert!(output.contains("detail_correlation="), "the bounded digest must remain: {output}");
        assert!(!output.contains("session-alpha"), "the raw session id must not reach the log: {output}");
        assert_detail_is_absent(&output);
    }

    /// Assert the rate-limit warning carries fixed fields only.
    #[test]
    fn notice_publish_failure_logs_fixed_fields_only() {
        let correlation = session_correlation("raw-session-identifier");
        let output = capture(|| {
            log_notice_publish_failure(&std::io::Error::other(ADVERSARIAL_DETAIL), &correlation);
        });

        assert!(output.contains("operation=\"provider_rate_limit_notice\""), "{output}");
        assert!(output.contains("error_category=\"rate_limit_notice_publish_failed\""), "{output}");
        assert!(output.contains("stage=\"publish_rate_limit_notice\""), "{output}");
        assert!(output.contains(&format!("session_correlation={correlation}")), "{output}");
        assert!(output.contains(&format!("error_bytes={}", ADVERSARIAL_DETAIL.len())), "{output}");
        assert!(output.contains("detail_correlation="), "the bounded digest must remain: {output}");
        assert_detail_is_absent(&output);
    }

    /// Assert configured identities are logged as digests, never verbatim.
    #[test]
    fn identifier_correlations_hide_the_identifier_and_stay_bounded() {
        let model = "vendor/model-with-a-very-long-and-unbounded-name\nwith-controls";
        let correlation = identifier_correlation(model);

        assert_eq!(correlation, identifier_correlation(model), "identical identities must correlate");
        assert_ne!(correlation, identifier_correlation("vendor/other-model"), "distinct identities must differ");
        assert_eq!(correlation.len(), CORRELATION_HEX_LENGTH, "the key has a fixed width");
        assert!(correlation.chars().all(|character| character.is_ascii_hexdigit()), "{correlation}");
        assert!(!correlation.contains("vendor"), "the identifier must not survive: {correlation}");
        assert!(!correlation.chars().any(char::is_control), "no control characters: {correlation}");
    }

    /// Assert correlation keys are stable, distinct, bounded and non-reversible.
    #[test]
    fn correlation_keys_are_stable_bounded_and_non_reversible() {
        let detail = "provider payload for alice@example.com";
        let first = detail_correlation(detail);
        let again = detail_correlation(detail);
        let other = detail_correlation("a different failure");

        assert_eq!(first, again, "identical failures must correlate");
        assert_ne!(first, other, "distinct failures must not collide");
        assert!(!first.contains("alice"), "the key must not carry the source text: {first}");
        assert_eq!(first.len(), CORRELATION_HEX_LENGTH, "the key has a fixed width");
        assert_eq!(session_correlation("session-alpha").len(), CORRELATION_HEX_LENGTH);
    }
}

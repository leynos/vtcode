//! Diagnostics for a failed Lody usage publication.
//!
//! The send error can embed provider payload, archive paths, or credentials, so
//! the warning this module exercises records a size and a digest instead of the
//! rendering, and a bounded correlation instead of the client-supplied session
//! identifier.

use super::*;
use std::time::{Duration, Instant};

/// Captured WARN output from a tracing subscriber.
struct CapturedWarn(Arc<std::sync::Mutex<Vec<u8>>>);

impl CapturedWarn {
    /// A new sink sharing the same buffer with any writer clone.
    fn new() -> Self {
        Self(Arc::new(std::sync::Mutex::new(Vec::new())))
    }

    /// A writer handle that appends to this sink's buffer.
    fn writer(&self) -> Self {
        Self(Arc::clone(&self.0))
    }

    /// The captured output as text.
    fn text(&self) -> String {
        let bytes = self.0.lock().expect("captured log mutex").clone();
        String::from_utf8(bytes).expect("captured logs are UTF-8")
    }
}

impl std::io::Write for CapturedWarn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("captured log mutex").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedWarn {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.writer()
    }
}

/// Interval between the two publication-clock reads in the failure case.
const FAILURE_STEP_MS: u64 = 275;

/// A usage-publication failure logs fixed fields and bounded values only.
///
/// The send error can embed provider payload, archive paths, or credentials,
/// so the warning records its size and a digest rather than its rendering. The
/// session is client-visible too, so the record carries a bounded correlation
/// instead of the identifier the client supplied.
///
/// The two instants are the publication clock's own reads, so the reported
/// duration is a function of explicit inputs rather than of the wall clock.
#[test]
fn usage_publication_failure_logs_a_bounded_diagnostic() {
    let logs = CapturedWarn::new();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .with_writer(logs.writer())
        .finish();
    let metrics = diapason_core::metrics::MetricsCollector::new();
    let session_id = "session-9f3a-client-visible";
    let session_correlation = bounded_session_correlation(&acp::SessionId::new(Arc::from(session_id)));
    let detail = "transport rejected /home/user/private/session.json while forwarding sk-live-secret";

    let started_at = Instant::now();
    tracing::subscriber::with_default(subscriber, || {
        let span = info_span!("acp_lody_usage_notification");
        let _entered = span.enter();
        record_usage_failure(
            &span,
            started_at,
            started_at + Duration::from_millis(FAILURE_STEP_MS),
            &metrics,
            "notification_send_failed",
            &session_correlation,
            &detail,
        );
    });

    let captured = logs.text();
    assert!(
        captured.contains("Failed to publish Lody ACP usage update"),
        "the failure must be reported: {captured}"
    );
    assert!(
        captured.contains(&format!("session_correlation=\"{session_correlation}\"")),
        "the bounded session correlation must be recorded: {captured}"
    );
    assert!(
        captured.contains("error_category=\"notification_send_failed\""),
        "the closed-set category must be recorded: {captured}"
    );
    assert!(
        captured.contains(&format!("error_bytes={}", detail.len())),
        "the error size must be measured rather than rendered: {captured}"
    );
    assert!(
        captured.contains(&format!("elapsed_ms={FAILURE_STEP_MS}")),
        "the failure duration must be the injected interval, not the wall clock: {captured}"
    );
    assert!(
        captured.contains(&detail_correlation(detail)),
        "the bounded detail digest must be recorded: {captured}"
    );
    assert!(!captured.contains(session_id), "the raw session id must not be logged: {captured}");
    assert!(
        !captured.contains("/home/user/private/session.json"),
        "the error rendering must not be logged: {captured}"
    );
    assert!(!captured.contains("sk-live-secret"), "the error rendering must not be logged: {captured}");
}

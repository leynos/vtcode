//! Covers the fixed-field ACP diagnostics boundary.
//!
//! Hook text, workspace paths, provider payloads, and transport detail are
//! untrusted, so these tests pin exactly which fields reach each severity.

use super::{log_suppressed_error, warn_hook_block_reason, warn_hook_message, warn_workspace_mcp_shutdown_failure};
use diapason_core::hooks::HookMessage;
use std::fmt::Write as FmtWrite;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

#[derive(Clone, Default)]
struct HookLogCapture(Arc<Mutex<Vec<String>>>);

impl<S> Layer<S> for HookLogCapture
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        let mut fields = CapturedFields::default();
        event.record(&mut fields);
        self.0.lock().expect("hook log capture lock").push(fields.0);
    }
}

#[derive(Default)]
struct CapturedFields(String);

impl Visit for CapturedFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        write!(&mut self.0, " {}={value}", field.name()).expect("write trace field");
    }
}

#[test]
fn hook_logs_keep_event_level_and_size_without_hook_text() {
    let private_path = "/home/private/workspace";
    let private_message = format!("hook stdout from {private_path}");
    let private_reason = format!("blocked because of {private_path}");
    let capture = HookLogCapture::default();
    let events = Arc::clone(&capture.0);
    let subscriber = Registry::default().with(capture);

    tracing::subscriber::with_default(subscriber, || {
        warn_hook_message("PreToolUse", &HookMessage::warning(private_message.clone()));
        warn_hook_block_reason("UserPromptSubmit", &private_reason);
        warn_workspace_mcp_shutdown_failure(&anyhow::anyhow!("shutdown failed for {private_path}"));
    });

    let events = events.lock().expect("hook log capture lock");
    assert_eq!(events.len(), 3);
    let captured = events.join("\n");
    assert!(!captured.contains(private_message.as_str()), "hook message text must stay out of logs");
    assert!(!captured.contains(private_reason.as_str()), "hook block reasons must stay out of logs");
    assert!(!captured.contains(private_path), "workspace paths must stay out of logs");
    assert!(captured.contains("hook_event=\"PreToolUse\""));
    assert!(captured.contains("level=Warning"));
    assert!(captured.contains(&format!("message_bytes={}", private_message.len())));
    assert!(captured.contains("hook_event=\"UserPromptSubmit\""));
    assert!(captured.contains(&format!("reason_bytes={}", private_reason.len())));
    assert!(captured.contains("error_category=\"mcp_client_shutdown\""));
    assert!(captured.contains("error_chain_depth=1"));
}

#[test]
fn suppressed_error_keeps_fixed_fields_and_never_logs_untrusted_text() {
    let capture = HookLogCapture::default();
    let events = Arc::clone(&capture.0);
    let subscriber = Registry::default().with(capture);
    let error = anyhow::anyhow!(
        "archive write failed for /home/private/archive.json after POST https://api.example.invalid/v1/chat \
         with alice@example.com and raw-session-identifier"
    )
    .context("checkpointing ACP session");

    tracing::subscriber::with_default(subscriber, || {
        log_suppressed_error(&error, "session_checkpoint", "archive_write_failed");
    });

    let events = events.lock().expect("hook log capture lock");
    assert_eq!(events.len(), 1);
    let captured = &events[0];
    assert!(captured.contains("operation=\"session_checkpoint\""));
    assert!(captured.contains("error_category=\"archive_write_failed\""));
    let outer_message = "checkpointing ACP session";
    assert!(
        captured.contains(&format!("error_bytes={}", outer_message.len())),
        "the outer message size is retained, got {captured}"
    );
    assert!(
        captured.contains("detail_correlation="),
        "the bounded correlation digest is retained, got {captured}"
    );
    for leaked in [
        "checkpointing ACP session",
        "archive write failed",
        "/home/private",
        "api.example.invalid",
        "alice@example.com",
        "raw-session-identifier",
    ] {
        assert!(!captured.contains(leaked), "{leaked} must never be logged, got {captured}");
    }
}

#[test]
fn suppressed_error_detail_stays_out_of_default_log_levels() {
    let capture = HookLogCapture::default();
    let events = Arc::clone(&capture.0);
    let subscriber = Registry::default()
        .with(tracing_subscriber::filter::LevelFilter::INFO)
        .with(capture);
    let private_path = "/home/private/workspace";

    tracing::subscriber::with_default(subscriber, || {
        log_suppressed_error(&anyhow::anyhow!("failed to read {private_path}"), "session_list", "archive_read_failed");
    });

    let events = events.lock().expect("hook log capture lock");
    assert!(events.is_empty(), "suppressed detail must not reach warn/error output, got {events:?}");
}

#[test]
fn session_runtime_failure_reports_a_stable_reason_without_the_error_chain() {
    let capture = HookLogCapture::default();
    let events = Arc::clone(&capture.0);
    // Capture only operator-visible severities: the full chain stays at
    // debug by design, so asserting on it here would contradict the
    // suppressed-detail convention this boundary follows.
    let subscriber = Registry::default()
        .with(tracing_subscriber::filter::LevelFilter::WARN)
        .with(capture);
    let private_context = "/home/private/workspace/session-archive";
    let error = anyhow::anyhow!("poisoned lock for {private_context}").context("reading ACP session workspace runtime");

    let message = tracing::subscriber::with_default(subscriber, || super::report_session_runtime_failure(&error));

    assert_eq!(
        message,
        crate::zed::constants::SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE,
        "the client-facing failure must be a fixed message"
    );
    assert!(!message.contains(private_context), "the failure must not echo workspace detail");

    let events = events.lock().expect("hook log capture lock");
    let captured = events.join("\n");
    assert!(!captured.contains(private_context), "workspace paths must stay out of logs");
    assert!(!captured.contains("poisoned lock for"), "raw error text must stay out of warnings");
    assert!(captured.contains("operation=\"session_workspace_runtime_read\""));
    assert!(captured.contains("component=\"session_workspace_runtime\""));
    assert!(captured.contains("outcome=\"failed\""));
    assert!(captured.contains("error_category=\"session_lock_unavailable\""));
    assert!(captured.contains("error_chain_depth=2"));
}

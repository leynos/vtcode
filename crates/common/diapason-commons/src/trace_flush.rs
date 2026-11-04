#![expect(
    clippy::let_underscore_must_use,
    reason = "The trace flush hook is registered once and duplicate registration is intentionally ignored."
)]

//! Global trace log flush hook.
//!
//! Allows any crate (including `diapason-ui`) to trigger a trace log flush
//! without depending on `diapason-core`. The flush callback is registered once
//! during tracing initialization and can be invoked from signal handlers or
//! shutdown sequences.

use std::sync::OnceLock;

static FLUSH_HOOK: OnceLock<fn()> = OnceLock::new();

/// Register a flush callback. Called once during tracing initialization.
pub fn register_trace_flush_hook(f: fn()) {
    let _ = FLUSH_HOOK.set(f);
}

/// Flush the global trace log writer.
///
/// Safe to call from signal handlers, shutdown hooks, or `Drop` implementations.
/// No-op if no flush hook has been registered.
pub fn flush_trace_log() {
    if let Some(hook) = FLUSH_HOOK.get() {
        hook();
    }
}

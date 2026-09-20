//! Internal, opt-in startup timing shared by the binary and terminal UI.
//!
//! The recorder intentionally emits only phase names and durations. It is
//! initialized before tracing so early bootstrap phases are observable without
//! changing the normal logging setup.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

type FirstRenderHook = Box<dyn FnOnce() + Send + 'static>;

struct StartupTraceState {
    enabled: bool,
    started_at: Instant,
    first_render_seen: AtomicBool,
    first_render_hook: Mutex<Option<FirstRenderHook>>,
}

static STATE: OnceLock<StartupTraceState> = OnceLock::new();
static HOOK_STATE: AtomicU8 = AtomicU8::new(0);

fn state() -> &'static StartupTraceState {
    STATE.get_or_init(|| StartupTraceState {
        enabled: std::env::var("VTCODE_STARTUP_TRACE").is_ok_and(|value| value == "1"),
        started_at: Instant::now(),
        first_render_seen: AtomicBool::new(false),
        first_render_hook: Mutex::new(None),
    })
}

/// Initialize startup timing before tracing is configured.
pub fn initialize() {
    let _state = state();
}

/// Start a phase timer when startup tracing is enabled.
#[must_use]
pub fn phase_started() -> Option<Instant> {
    state().enabled.then(Instant::now)
}

/// Record a completed phase as a duration in milliseconds.
pub fn record_phase(phase: &str, started_at: Option<Instant>) {
    let Some(started_at) = started_at else {
        return;
    };
    let elapsed_ms = started_at.elapsed().as_secs_f64() * 1_000.0;
    eprintln!("VTCODE_STARTUP_TRACE phase={phase} duration_ms={elapsed_ms:.3}");
}

/// Record a process-level startup milestone without requiring a phase timer.
/// This is useful for short-lived commands whose output is the completion
/// boundary rather than an interactive frame.
pub fn record_milestone(name: &str) {
    let state = state();
    if state.enabled {
        let elapsed_ms = state.started_at.elapsed().as_secs_f64() * 1_000.0;
        eprintln!("VTCODE_STARTUP_TRACE phase={name} elapsed_ms={elapsed_ms:.3}");
    }
}

/// Record a phase using an elapsed duration supplied by a caller that already
/// owns a timer. This keeps startup tracing usable across crate boundaries.
pub fn record_duration(name: &str, duration: std::time::Duration) {
    if state().enabled {
        eprintln!("VTCODE_STARTUP_TRACE phase={name} duration_ms={:.3}", duration.as_secs_f64() * 1_000.0);
    }
}

/// Install work that should run after the first interactive frame is drawn.
///
/// The hook is also installed when tracing is disabled because the startup
/// prompt-size warning uses the same first-render boundary. Hooks are
/// one-shot and are safe to register after the frame in tests or embedded
/// integrations.
pub fn install_first_render_hook<F>(hook: F)
where
    F: FnOnce() + Send + 'static,
{
    let state = state();
    if state.first_render_seen.load(Ordering::Acquire) {
        hook();
        return;
    }

    let mut pending = state.first_render_hook.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.first_render_seen.load(Ordering::Acquire) {
        drop(pending);
        hook();
    } else {
        *pending = Some(Box::new(hook));
        HOOK_STATE.store(1, Ordering::Release);
    }
}

/// Record and publish the first interactive frame boundary.
pub fn record_first_render() {
    let state = state();
    if !state.enabled && HOOK_STATE.load(Ordering::Acquire) == 0 {
        if !state.first_render_seen.load(Ordering::Acquire) {
            state.first_render_seen.store(true, Ordering::Release);
        }
        return;
    }
    if state.first_render_seen.swap(true, Ordering::AcqRel) {
        return;
    }

    if state.enabled {
        let first_render_elapsed_ms = state.started_at.elapsed().as_secs_f64() * 1_000.0;
        eprintln!("VTCODE_STARTUP_TRACE phase=first_ui_render duration_ms={first_render_elapsed_ms:.3}");
    }

    if HOOK_STATE.swap(0, Ordering::AcqRel) == 1 {
        let hook = state
            .first_render_hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(hook) = hook {
            hook();
        }
    }
}

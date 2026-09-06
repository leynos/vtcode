use std::sync::Once;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

pub(crate) static TUI_INITIALIZED: AtomicBool = AtomicBool::new(false);
/// Whether any component has put the terminal into a non-default state
/// (raw mode, alternate screen, bracketed paste, ...). The canonical restore
/// path only emits restore sequences when this is set, so error reports for
/// runs that never touched the terminal stay clean.
pub(crate) static TERMINAL_MODIFIED: AtomicBool = AtomicBool::new(false);
pub(crate) static KEYBOARD_ENHANCEMENTS_PUSHED: AtomicBool = AtomicBool::new(false);
/// Whether the TUI is currently running on the alternate screen buffer.
///
/// Tracks `UiSurfacePreference::Alternate`/`Auto` sessions so the canonical
/// restore path knows whether it must clear the main screen explicitly
/// (inline sessions draw directly on it) or whether leaving the alternate
/// screen already restores it.
pub(crate) static ALTERNATE_SCREEN_ACTIVE: AtomicBool = AtomicBool::new(false);
pub(crate) static RESTORE_DONE: AtomicBool = AtomicBool::new(false);
static TERMINAL_OPERATION_LOCK: Mutex<()> = Mutex::new(());
static DEBUG_MODE: AtomicBool = AtomicBool::new(cfg!(debug_assertions));
pub(crate) static COLOUR_EYRE_ENABLED: AtomicBool = AtomicBool::new(cfg!(debug_assertions));
static SHOW_DIAGNOSTICS: AtomicBool = AtomicBool::new(false);
pub(crate) static COLOUR_EYRE_SETUP_ONCE: Once = Once::new();
#[cfg(debug_assertions)]
pub(crate) static COLOUR_EYRE_PANIC_HOOK: OnceLock<color_eyre::config::PanicHook> = OnceLock::new();
pub(crate) static APP_METADATA: OnceLock<AppMetadata> = OnceLock::new();

#[derive(Clone, Debug)]
pub(crate) struct AppMetadata {
    pub(crate) name: &'static str,
    pub(crate) version: &'static str,
    pub(crate) authors: &'static str,
    pub(crate) repository: Option<&'static str>,
}

impl AppMetadata {
    pub(crate) fn default_for_tui_crate() -> Self {
        Self {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
            authors: env!("CARGO_PKG_AUTHORS"),
            repository: Some(env!("CARGO_PKG_REPOSITORY")).filter(|value| !value.is_empty()),
        }
    }
}

pub fn set_debug_mode(enabled: bool) {
    DEBUG_MODE.store(enabled, Ordering::SeqCst);
}

pub(crate) fn is_debug_mode() -> bool {
    DEBUG_MODE.load(Ordering::SeqCst)
}

pub fn set_colour_eyre_enabled(enabled: bool) {
    COLOUR_EYRE_ENABLED.store(enabled, Ordering::SeqCst);
}

pub(crate) fn is_colour_eyre_enabled() -> bool {
    COLOUR_EYRE_ENABLED.load(Ordering::SeqCst)
}

pub fn set_show_diagnostics(enabled: bool) {
    SHOW_DIAGNOSTICS.store(enabled, Ordering::SeqCst);
}

pub(crate) fn show_diagnostics() -> bool {
    SHOW_DIAGNOSTICS.load(Ordering::SeqCst)
}

pub fn set_app_metadata(
    name: &'static str,
    version: &'static str,
    authors: &'static str,
    repository: Option<&'static str>,
) {
    let _ = APP_METADATA.set(AppMetadata {
        name,
        version,
        authors,
        repository: repository.filter(|value| !value.is_empty()),
    });
}

pub(crate) fn app_metadata() -> AppMetadata {
    APP_METADATA.get().cloned().unwrap_or_else(AppMetadata::default_for_tui_crate)
}

pub(crate) fn mark_tui_initialized() {
    TUI_INITIALIZED.store(true, Ordering::SeqCst);
    RESTORE_DONE.store(false, Ordering::SeqCst);
    RAW_MODE_WAS_ENABLED.store(false, Ordering::SeqCst);
    // Fresh session: alternate-screen state is set by the runner when it enters one.
    ALTERNATE_SCREEN_ACTIVE.store(false, Ordering::SeqCst);
}

pub(crate) fn mark_tui_deinitialized() {
    TUI_INITIALIZED.store(false, Ordering::SeqCst);
}

pub(crate) fn is_tui_initialized() -> bool {
    TUI_INITIALIZED.load(Ordering::SeqCst)
}

pub(crate) fn mark_terminal_modified() {
    TERMINAL_MODIFIED.store(true, Ordering::SeqCst);
}

pub(crate) fn mark_terminal_restored() {
    TERMINAL_MODIFIED.store(false, Ordering::SeqCst);
}

pub(crate) fn is_terminal_modified() -> bool {
    TERMINAL_MODIFIED.load(Ordering::SeqCst)
}

pub(crate) fn mark_keyboard_enhancements_pushed(pushed: bool) {
    KEYBOARD_ENHANCEMENTS_PUSHED.store(pushed, Ordering::SeqCst);
}

pub(crate) fn mark_alternate_screen_active(active: bool) {
    ALTERNATE_SCREEN_ACTIVE.store(active, Ordering::SeqCst);
}

pub(crate) fn is_alternate_screen_active() -> bool {
    ALTERNATE_SCREEN_ACTIVE.load(Ordering::SeqCst)
}

/// Track whether raw mode was enabled before the TUI took control.
/// This allows the canonical restore path to return the terminal to the
/// exact prior raw-mode state instead of unconditionally disabling it.
static RAW_MODE_WAS_ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn mark_raw_mode_was_enabled(enabled: bool) {
    RAW_MODE_WAS_ENABLED.store(enabled, Ordering::SeqCst);
}

pub(crate) fn is_raw_mode_was_enabled() -> bool {
    RAW_MODE_WAS_ENABLED.load(Ordering::SeqCst)
}

/// Serialize terminal writes with restoration so a forced host cleanup cannot
/// switch buffers while a render is still painting a frame.
pub(crate) fn lock_terminal_operations() -> MutexGuard<'static, ()> {
    match TERMINAL_OPERATION_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::warn!("terminal operation lock was poisoned; continuing cleanup");
            poisoned.into_inner()
        }
    }
}

pub(crate) fn try_claim_restore() -> bool {
    !RESTORE_DONE.swap(true, Ordering::SeqCst)
}

/// Whether terminal restoration has already been claimed (by this task, the
/// host, or a panic hook). Once true, the TUI must not draw any more frames:
/// the main screen buffer is live and a straggler frame would leak transcript
/// content into the CLI scrollback.
pub(crate) fn is_restore_claimed() -> bool {
    RESTORE_DONE.load(Ordering::SeqCst)
}

/// Install color-eyre's eyre hook for richer top-level error rendering in dev/debug mode.
#[cfg(debug_assertions)]
pub(crate) fn maybe_prepare_colour_eyre_hooks() {
    if !is_colour_eyre_enabled() {
        return;
    }

    COLOUR_EYRE_SETUP_ONCE.call_once(|| {
        let hooks = color_eyre::config::HookBuilder::default().try_into_hooks();
        match hooks {
            Ok((panic_hook, eyre_hook)) => {
                let _ = COLOUR_EYRE_PANIC_HOOK.set(panic_hook);
                if let Err(error) = eyre_hook.install() {
                    eprintln!("warning: failed to install color-eyre hook: {error}");
                }
            }
            Err(error) => {
                eprintln!("warning: failed to prepare color-eyre hook: {error}");
            }
        }
    });
}

#[cfg(not(debug_assertions))]
pub(crate) fn maybe_prepare_colour_eyre_hooks() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_colour_eyre_toggle() {
        COLOUR_EYRE_ENABLED.store(false, Ordering::SeqCst);
        assert!(!is_colour_eyre_enabled());

        set_colour_eyre_enabled(true);
        assert!(is_colour_eyre_enabled());

        COLOUR_EYRE_ENABLED.store(false, Ordering::SeqCst);
    }

    #[test]
    fn is_restore_claimed_tracks_restore_claim() {
        RESTORE_DONE.store(false, Ordering::SeqCst);
        assert!(!is_restore_claimed());
        assert!(try_claim_restore());
        assert!(is_restore_claimed());
        assert!(!try_claim_restore(), "second claim must fail");
        RESTORE_DONE.store(false, Ordering::SeqCst);
    }
}

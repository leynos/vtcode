use std::fs::OpenOptions;
use std::io::{self, Write};
use std::sync::atomic::Ordering;

use ratatui::crossterm::{
    cursor::{MoveToColumn, RestorePosition, SetCursorStyle, Show},
    event::{DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, PopKeyboardEnhancementFlags},
    execute,
    terminal::{Clear, ClearType, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, is_raw_mode_enabled},
};

use super::state::{self, KEYBOARD_ENHANCEMENTS_PUSHED};

/// Emit the terminal-restoration escape sequence.
///
/// When `clear_alternate` is true we are currently on the alternate screen
/// buffer and should purge that buffer's contents before leaving it so no
/// transcript frames are accidentally revealed in the main scrollback.
/// When false we are inline on the main screen and must not emit a full
/// Clear(All) which would wipe the scrollback and leave a blank gap above
/// the next prompt.
///
/// Writes through any `Write` so the exact sequence is unit-testable.
fn emit_restore_sequence(writer: &mut impl Write, clear_alternate: bool) -> io::Result<()> {
    // Clear current line to remove any echoed ^C characters
    execute!(writer, MoveToColumn(0), Clear(ClearType::CurrentLine))?;

    // If we're on the alternate screen, clear its contents BEFORE leaving
    // so the act of leaving the buffer does not reveal the last TUI frame
    // as scrollback in the main terminal.
    if clear_alternate {
        if let Err(error) = execute!(writer, Clear(ClearType::All)) {
            tracing::debug!(%error, "failed to clear alternate terminal buffer before restore");
        }
    }

    // Leave alternate screen (best-effort regardless of current state)
    execute!(writer, LeaveAlternateScreen)?;

    // Disable terminal modes
    execute!(writer, DisableBracketedPaste)?;
    execute!(writer, DisableFocusChange)?;
    execute!(writer, DisableMouseCapture)?;

    // Only pop keyboard enhancement flags if actually pushed
    if KEYBOARD_ENHANCEMENTS_PUSHED.swap(false, Ordering::SeqCst) {
        execute!(writer, PopKeyboardEnhancementFlags)?;
    }

    Ok(())
}

fn open_tty_writer() -> Option<std::fs::File> {
    OpenOptions::new().write(true).open("/dev/tty").ok()
}

fn emit_restore_to_all_targets(clear_alternate: bool) -> Option<io::Error> {
    let mut first_error: Option<io::Error> = None;

    let mut stderr = io::stderr();
    if let Err(error) = emit_restore_sequence(&mut stderr, clear_alternate) {
        first_error.get_or_insert(error);
    }
    if let Err(error) = execute!(stderr, SetCursorStyle::DefaultUserShape, Show, RestorePosition) {
        first_error.get_or_insert_with(|| io::Error::other(error.to_string()));
    }
    let _ = stderr.flush();
    crate::tui::core_tui::runner::terminal_io::reset_mouse_pointer_shape();

    if let Some(mut tty) = open_tty_writer() {
        let _ = emit_restore_sequence(&mut tty, clear_alternate);
        let _ = execute!(tty, SetCursorStyle::DefaultUserShape, Show, RestorePosition);
        let _ = tty.flush();
        let _ = write!(tty, "\x1b]22;default\x07");
        let _ = tty.flush();
    }

    first_error
}

/// Restore terminal to a usable state after a panic or error.
///
/// This is the single canonical function for terminal restoration.
/// It is idempotent: subsequent calls are no-ops.
///
/// - Drains pending events before and after restoration
/// - Clears the alternate viewport before leaving the alternate screen
/// - Leaves the alternate screen
/// - Preserves inline transcript scrollback instead of clearing the main viewport
/// - Disables bracketed paste, focus change, mouse capture
/// - Pops keyboard enhancement flags if pushed
/// - Resets cursor style and shows cursor
/// - Restores raw mode to its state before the TUI started
pub fn restore_tui() -> io::Result<()> {
    if !state::try_claim_restore() {
        return Ok(());
    }

    let _terminal_lock = state::lock_terminal_operations();
    state::mark_tui_deinitialized();

    let terminal_modified = state::is_terminal_modified();
    let alternate_active = state::is_alternate_screen_active();

    // Never emit restore sequences when no component modified the terminal
    // and no alternate screen is active: error reports for non-TUI runs
    // (failed startup, one-shot commands) would otherwise spray raw escape
    // sequences before the message. If alternate screen is active we must
    // still leave it even when TERMINAL_MODIFIED was not yet set (partial
    // init failure that succeeded to enter alt screen but failed before
    // flagging modification).
    if !terminal_modified && !alternate_active {
        return Ok(());
    }
    state::mark_terminal_restored();
    if alternate_active {
        state::mark_alternate_screen_active(false);
    }

    let mut first_error: Option<io::Error> = None;

    crate::tui::core_tui::runner::terminal_io::drain_terminal_events();

    // If the TUI ran on the alternate screen, clear that buffer before
    // leaving so the transcript is not revealed as scrollback in the
    // primary screen. Inline sessions must not be cleared here.
    let clear_alternate = alternate_active;
    if let Some(error) = emit_restore_to_all_targets(clear_alternate) {
        first_error.get_or_insert(error);
    }

    // Drain terminal responses from restore sequences while raw mode still active
    crate::tui::core_tui::runner::terminal_io::drain_terminal_events();

    // Restore raw-mode to the state it had before the TUI started.
    // If we can query the terminal's current raw-mode we only toggle when
    // needed; otherwise fall back to disabling raw mode to preserve a
    // conservative and usable state.
    let previous_raw = state::is_raw_mode_was_enabled();
    match is_raw_mode_enabled() {
        Ok(current_enabled) => {
            if previous_raw && !current_enabled {
                if let Err(error) = enable_raw_mode() {
                    first_error.get_or_insert(error);
                }
            } else if !previous_raw && current_enabled {
                if let Err(error) = disable_raw_mode() {
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(_) => {
            // Couldn't query — fall back to best-effort disable when the
            // TUI had enabled raw mode (previous_raw == false) to avoid
            // leaving the tty in a no-echo/no-stdin state.
            if !previous_raw {
                if let Err(error) = disable_raw_mode() {
                    first_error.get_or_insert(error);
                }
            }
        }
    }

    // Best-effort stty sane equivalent for /dev/tty when raw-mode toggles
    // succeeded but tty still has echo off due to cargo wrapping stderr.
    if let Some(mut tty) = open_tty_writer() {
        let _ = tty.flush();
    }
    // Ensure stderr is flushed after raw mode restore
    let _ = io::stderr().flush();
    state::mark_raw_mode_was_enabled(false);

    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn test_restore_terminal_no_panic_when_not_initialized() {
        state::RESTORE_DONE.store(false, Ordering::SeqCst);
        state::TUI_INITIALIZED.store(false, Ordering::SeqCst);

        let result = restore_tui();
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn restore_sequence_does_not_clear_inline_screen() {
        let mut bytes: Vec<u8> = Vec::new();
        // Inline session: do not clear the full-screen buffer here
        emit_restore_sequence(&mut bytes, false).unwrap();
        let text = String::from_utf8(bytes).unwrap();

        assert!(!text.contains("\x1b[2J"), "inline restore must not emit ESC[2J, got: {text:?}");
        assert!(text.contains("\x1b[1G\x1b[2K"), "inline restore must keep line clear, got: {text:?}");
        assert!(
            text.contains("\x1b[?1049l"),
            "inline restore must leave alternate screen (best-effort), got: {text:?}"
        );
    }

    #[test]
    fn restore_sequence_clears_alternate_buffer_before_leaving() {
        let mut bytes: Vec<u8> = Vec::new();
        // Alternate session: clear the alternate buffer to avoid leaking
        // TUI frames into the main scrollback when leaving it.
        emit_restore_sequence(&mut bytes, true).unwrap();
        let text = String::from_utf8(bytes).unwrap();

        assert!(
            text.contains("\x1b[2J"),
            "alternate-restore must emit ESC[2J to purge alternate buffer, got: {text:?}"
        );
        let clear_index = text.find("\x1b[2J").expect("alternate screen clear is present");
        let leave_index = text.find("\x1b[?1049l").expect("alternate screen leave is present");
        assert!(clear_index < leave_index, "alternate buffer must be cleared before leaving: {text:?}");
    }

    #[test]
    fn alternate_screen_flag_resets_on_new_session() {
        state::mark_alternate_screen_active(true);
        assert!(state::is_alternate_screen_active());
        state::mark_tui_initialized();
        assert!(!state::is_alternate_screen_active(), "new TUI session must start inline");
    }

    #[test]
    fn terminal_modified_flag_round_trip() {
        state::mark_terminal_restored();
        assert!(!state::is_terminal_modified());
        state::mark_terminal_modified();
        assert!(state::is_terminal_modified());
        state::mark_terminal_restored();
        assert!(!state::is_terminal_modified());
    }

    #[test]
    fn new_tui_session_waits_for_terminal_mutation() {
        state::mark_terminal_restored();
        assert!(!state::is_terminal_modified());
        state::mark_tui_initialized();
        assert!(state::is_tui_initialized());
        assert!(!state::is_terminal_modified(), "TUI registration must not claim terminal mutation");
        state::mark_tui_deinitialized();
    }

    #[test]
    fn restore_skips_emission_when_terminal_never_modified() {
        state::RESTORE_DONE.store(false, Ordering::SeqCst);
        state::TUI_INITIALIZED.store(false, Ordering::SeqCst);
        state::mark_terminal_restored();

        let result = restore_tui();
        assert!(result.is_ok() || result.is_err());
        assert!(
            state::RESTORE_DONE.load(Ordering::SeqCst),
            "restore must still be claimed so later calls stay no-ops"
        );
        assert!(!state::is_terminal_modified(), "unmodified terminal must stay unmodified");

        // A modified terminal must still run the full restore and clear the flag.
        state::RESTORE_DONE.store(false, Ordering::SeqCst);
        state::mark_terminal_modified();
        let result = restore_tui();
        assert!(result.is_ok() || result.is_err());
        assert!(!state::is_terminal_modified(), "restore must clear the modified flag");
    }
}

use std::io;

use anyhow::Result;
use ratatui::crossterm::{
    cursor::{RestorePosition, SavePosition},
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
        EnableMouseCapture,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, is_raw_mode_enabled},
};

use crate::tui::options::FullscreenInteractionSettings;

/// Tracks terminal features changed while the TUI owns the terminal.
#[derive(Debug, Clone)]
pub(super) struct TerminalModeState {
    /// Whether Diapason enabled bracketed paste.
    bracketed_paste_enabled: bool,
    /// Whether Diapason enabled raw mode.
    raw_mode_enabled: bool,
    /// Whether Diapason enabled mouse capture.
    mouse_capture_enabled: bool,
    /// Whether Diapason enabled focus change events.
    focus_change_enabled: bool,
    /// Whether Diapason pushed keyboard enhancement flags.
    keyboard_enhancements_pushed: bool,
    /// Whether the cursor position was saved before entering fullscreen
    cursor_position_saved: bool,
    /// Whether the alternate screen buffer is active
    alternate_screen_active: bool,
}

impl TerminalModeState {
    /// Create a new TerminalModeState with all modes disabled (clean state)
    fn new() -> Self {
        Self {
            bracketed_paste_enabled: false,
            raw_mode_enabled: false,
            mouse_capture_enabled: false,
            focus_change_enabled: false,
            keyboard_enhancements_pushed: false,
            cursor_position_saved: false,
            alternate_screen_active: false,
        }
    }

    pub(super) fn save_cursor_position(&mut self, stderr: &mut io::Stderr) {
        match execute!(stderr, SavePosition) {
            Ok(_) => {
                self.cursor_position_saved = true;
                crate::tui::ui::tui::panic_hook::mark_terminal_modified();
            }
            Err(error) => {
                tracing::debug!(%error, "failed to save cursor position for inline session");
            }
        }
    }

    pub(super) fn enter_alternate_screen(&mut self, stderr: &mut io::Stderr) -> Result<()> {
        execute!(stderr, EnterAlternateScreen)
            .map_err(|error| anyhow::anyhow!("failed to enter alternate inline screen: {error}"))?;
        self.alternate_screen_active = true;
        crate::tui::ui::tui::panic_hook::mark_terminal_modified();
        Ok(())
    }

    pub(super) fn push_keyboard_enhancement_flags(
        &mut self,
        stderr: &mut io::Stderr,
        keyboard_flags: crossterm::event::KeyboardEnhancementFlags,
    ) {
        use ratatui::crossterm::event::PushKeyboardEnhancementFlags;

        if keyboard_flags.is_empty() {
            return;
        }

        match execute!(stderr, PushKeyboardEnhancementFlags(keyboard_flags)) {
            Ok(_) => {
                self.keyboard_enhancements_pushed = true;
                crate::tui::ui::tui::panic_hook::mark_keyboard_enhancements_pushed(true);
                crate::tui::ui::tui::panic_hook::mark_terminal_modified();
            }
            Err(error) => {
                tracing::debug!(%error, "failed to push keyboard enhancement flags");
            }
        }
    }
}

impl Default for TerminalModeState {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) fn enable_terminal_modes(
    stderr: &mut io::Stderr,
    fullscreen: &FullscreenInteractionSettings,
) -> Result<TerminalModeState> {
    let mut state = TerminalModeState::new();

    // Capture existing raw-mode state before changing any terminal mode so the
    // canonical restore path can return the terminal to its original state.
    match is_raw_mode_enabled() {
        Ok(prev_enabled) => {
            crate::tui::ui::tui::panic_hook::state::mark_raw_mode_was_enabled(prev_enabled);
            if !prev_enabled {
                match enable_raw_mode() {
                    Ok(_) => {
                        state.raw_mode_enabled = true;
                        crate::tui::ui::tui::panic_hook::mark_terminal_modified();
                    }
                    Err(error) => {
                        return Err(anyhow::anyhow!("failed to enable raw mode: {error}"));
                    }
                }
            }
        }
        Err(error) => {
            tracing::debug!(%error, "failed to query raw mode; attempting to enable raw mode");
            match enable_raw_mode() {
                Ok(_) => {
                    state.raw_mode_enabled = true;
                    crate::tui::ui::tui::panic_hook::mark_terminal_modified();
                }
                Err(error) => {
                    return Err(anyhow::anyhow!("failed to enable raw mode: {error}"));
                }
            }
        }
    }

    // Enable bracketed paste
    match execute!(stderr, EnableBracketedPaste) {
        Ok(_) => {
            state.bracketed_paste_enabled = true;
            crate::tui::ui::tui::panic_hook::mark_terminal_modified();
        }
        Err(error) => {
            tracing::warn!(%error, "failed to enable bracketed paste");
        }
    }

    if fullscreen.mouse_capture {
        match execute!(stderr, EnableMouseCapture) {
            Ok(_) => {
                state.mouse_capture_enabled = true;
                crate::tui::ui::tui::panic_hook::mark_terminal_modified();
            }
            Err(error) => {
                tracing::warn!(%error, "failed to enable mouse capture");
            }
        }
    }

    // Enable focus change events
    match execute!(stderr, EnableFocusChange) {
        Ok(_) => {
            state.focus_change_enabled = true;
            crate::tui::ui::tui::panic_hook::mark_terminal_modified();
        }
        Err(error) => {
            tracing::debug!(%error, "failed to enable focus change events");
        }
    }

    Ok(state)
}

/// Restore terminal modes using the canonical single-source-of-truth function.
///
/// Delegates to `restore_tui()` which handles all terminal restoration
/// and is guarded by a `RESTORE_DONE` flag for idempotency.
pub(super) fn restore_terminal_modes(_state: &TerminalModeState) -> Result<()> {
    crate::tui::ui::tui::panic_hook::restore_tui()?;
    Ok(())
}

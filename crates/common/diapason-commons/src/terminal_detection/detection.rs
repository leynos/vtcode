//! Environment-driven terminal detection with an injectable environment reader.

use std::env;

use anyhow::Result;

use super::TerminalType;

impl TerminalType {
    /// Detects the current terminal emulator from its environment markers.
    ///
    /// # Errors
    ///
    /// This retains the fallible detection API for callers. Unavailable and
    /// non-Unicode environment variables are treated as absent markers.
    pub fn detect() -> Result<Self> {
        if let Ok(term_program) = env::var("TERM_PROGRAM")
            && let Some(terminal) = terminal_from_term_program(&term_program)
        {
            return Ok(terminal);
        }

        if env::var("KITTY_WINDOW_ID").is_ok() || env::var("KITTY_PID").is_ok() {
            return Ok(TerminalType::Kitty);
        }

        if env::var("ALACRITTY_SOCKET").is_ok() || env::var("ALACRITTY_LOG").is_ok() {
            return Ok(TerminalType::Alacritty);
        }

        if env::var("ZED_TERMINAL").is_ok() {
            return Ok(TerminalType::Zed);
        }

        if env::var("WT_SESSION").is_ok() || env::var("WT_PROFILE_ID").is_ok() {
            return Ok(TerminalType::WindowsTerminal);
        }

        if let Ok(term) = env::var("TERM")
            && let Some(terminal) = terminal_from_term(&term)
        {
            return Ok(terminal);
        }

        Ok(TerminalType::Unknown)
    }
}

/// Returns the terminal identified by a `TERM_PROGRAM` value, if any.
fn terminal_from_term_program(term_program: &str) -> Option<TerminalType> {
    const MARKERS: &[(&str, TerminalType)] = &[
        ("ghostty", TerminalType::Ghostty),
        ("wezterm", TerminalType::WezTerm),
        ("apple_terminal", TerminalType::TerminalApp),
        ("iterm", TerminalType::ITerm2),
        ("vscode", TerminalType::VSCode),
        ("warp", TerminalType::Warp),
        ("hyper", TerminalType::Hyper),
        ("tabby", TerminalType::Tabby),
    ];

    terminal_from_markers(term_program, MARKERS)
}

/// Returns the terminal identified by a `TERM` value, if any.
fn terminal_from_term(term: &str) -> Option<TerminalType> {
    const MARKERS: &[(&str, TerminalType)] = &[
        ("kitty", TerminalType::Kitty),
        ("alacritty", TerminalType::Alacritty),
        ("xterm", TerminalType::Xterm),
    ];

    terminal_from_markers(term, MARKERS)
}

/// Returns the first terminal whose marker appears in `value`, case-insensitively.
fn terminal_from_markers(value: &str, markers: &[(&str, TerminalType)]) -> Option<TerminalType> {
    let value_lower = value.to_lowercase();
    markers
        .iter()
        .find_map(|&(marker, terminal)| value_lower.contains(marker).then_some(terminal))
}

/// Returns whether `term_program` or `term` identifies the Ghostty terminal.
pub fn is_ghostty_terminal(term_program: Option<&str>, term: Option<&str>) -> bool {
    terminal_name_contains(term_program, "ghostty") || terminal_name_contains(term, "ghostty")
}

fn terminal_name_contains(value: Option<&str>, needle: &str) -> bool {
    value.is_some_and(|value| value.to_ascii_lowercase().contains(needle))
}

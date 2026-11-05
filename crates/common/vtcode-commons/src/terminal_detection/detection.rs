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
        if let Ok(term_program) = env::var("TERM_PROGRAM") {
            let term_lower = term_program.to_lowercase();

            if term_lower.contains("ghostty") {
                return Ok(TerminalType::Ghostty);
            } else if term_lower.contains("wezterm") {
                return Ok(TerminalType::WezTerm);
            } else if term_lower.contains("apple_terminal") {
                return Ok(TerminalType::TerminalApp);
            } else if term_lower.contains("iterm") {
                return Ok(TerminalType::ITerm2);
            } else if term_lower.contains("vscode") {
                return Ok(TerminalType::VSCode);
            } else if term_lower.contains("warp") {
                return Ok(TerminalType::Warp);
            } else if term_lower.contains("hyper") {
                return Ok(TerminalType::Hyper);
            } else if term_lower.contains("tabby") {
                return Ok(TerminalType::Tabby);
            }
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

        if let Ok(term) = env::var("TERM") {
            let term_lower = term.to_lowercase();

            if term_lower.contains("kitty") {
                return Ok(TerminalType::Kitty);
            } else if term_lower.contains("alacritty") {
                return Ok(TerminalType::Alacritty);
            } else if term_lower.contains("xterm") {
                return Ok(TerminalType::Xterm);
            }
        }

        Ok(TerminalType::Unknown)
    }
}

/// Returns the terminal identified by `environment`.
pub fn is_ghostty_terminal(term_program: Option<&str>, term: Option<&str>) -> bool {
    terminal_name_contains(term_program, "ghostty") || terminal_name_contains(term, "ghostty")
}

fn terminal_name_contains(value: Option<&str>, needle: &str) -> bool {
    value.map(|value| value.to_ascii_lowercase().contains(needle)).unwrap_or(false)
}

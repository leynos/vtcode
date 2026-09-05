//! TTY detection and capability utilities using crossterm's IsTty trait.
//!
//! This module provides safe and convenient TTY detection across the codebase,
//! abstracting away platform differences for TTY detection.
//!
//! # Usage
//!
//! ```rust
//! use vtcode_ui::tui::utils::tty::TtyExt;
//! use std::io;
//!
//! // Check if stdout is a TTY
//! if io::stdout().is_tty_ext() {
//!     // Apply terminal-specific features
//! }
//!
//! // Check if stdin is a TTY
//! if io::stdin().is_tty_ext() {
//!     // Interactive input available
//! }
//! ```

use crossterm::tty::IsTty;
use std::io;
use vtcode_commons::colour_policy::no_colour_env_active;

/// Extension trait for TTY detection on standard I/O streams.
///
/// This trait extends crossterm's `IsTty` to provide convenient methods
/// for checking TTY capabilities with better error handling.
pub trait TtyExt {
    /// Returns `true` if this stream is connected to a terminal.
    ///
    /// This is a convenience wrapper around crossterm's `IsTty` trait
    /// that provides consistent behaviour across the codebase.
    fn is_tty_ext(&self) -> bool;

    /// Returns `true` if this stream supports ANSI colour codes.
    ///
    /// This checks both TTY status and common environment variables
    /// that might disable colour output.
    fn supports_colour(&self) -> bool;

    /// Returns `true` if this stream supports interactive features.
    ///
    /// Interactive features include cursor movement, colour, and other
    /// terminal capabilities that require a real terminal.
    fn is_interactive(&self) -> bool;
}

impl TtyExt for io::Stdout {
    fn is_tty_ext(&self) -> bool {
        self.is_tty()
    }

    fn supports_colour(&self) -> bool {
        if !self.is_tty() {
            return false;
        }

        // Check NO_COLOR with strict non-empty semantics.
        if no_colour_env_active() {
            return false;
        }

        // Check for FORCE_COLOR environment variable
        if std::env::var_os("FORCE_COLOR").is_some() {
            return true;
        }

        true
    }

    fn is_interactive(&self) -> bool {
        self.is_tty() && self.supports_colour()
    }
}

impl TtyExt for io::Stderr {
    fn is_tty_ext(&self) -> bool {
        self.is_tty()
    }

    fn supports_colour(&self) -> bool {
        if !self.is_tty() {
            return false;
        }

        // Check NO_COLOR with strict non-empty semantics.
        if no_colour_env_active() {
            return false;
        }

        // Check for FORCE_COLOR environment variable
        if std::env::var_os("FORCE_COLOR").is_some() {
            return true;
        }

        true
    }

    fn is_interactive(&self) -> bool {
        self.is_tty() && self.supports_colour()
    }
}

impl TtyExt for io::Stdin {
    fn is_tty_ext(&self) -> bool {
        self.is_tty()
    }

    fn supports_colour(&self) -> bool {
        // Stdin doesn't output colour, but we check if it's interactive
        self.is_tty()
    }

    fn is_interactive(&self) -> bool {
        self.is_tty()
    }
}

/// TTY capabilities that can be queried for feature detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TtyCapabilities {
    /// Whether the terminal supports ANSI colour codes.
    colour: bool,
    /// Whether the terminal supports cursor movement and manipulation.
    cursor: bool,
    /// Whether the terminal supports bracketed paste mode.
    bracketed_paste: bool,
    /// Whether the terminal supports focus change events.
    focus_events: bool,
    /// Whether the terminal supports mouse input.
    mouse: bool,
    /// Whether the terminal supports keyboard enhancement flags.
    keyboard_enhancement: bool,
}

impl TtyCapabilities {
    /// Detect the capabilities of the current terminal.
    ///
    /// This function queries the terminal to determine which features
    /// are available. It should be called once at application startup
    /// and the results cached for later use.
    ///
    /// # Returns
    ///
    /// Returns `Some(TtyCapabilities)` if stderr is a TTY, otherwise `None`.
    pub(crate) fn detect() -> Option<Self> {
        let stderr = io::stderr();
        if !stderr.is_tty() {
            return None;
        }

        Some(Self {
            colour: stderr.supports_colour(),
            cursor: true,               // All TTYs support basic cursor movement
            bracketed_paste: true,      // Assume support, will fail gracefully if not
            focus_events: true,         // Assume support, will fail gracefully if not
            mouse: true,                // Assume support, will fail gracefully if not
            keyboard_enhancement: true, // Assume support, will fail gracefully if not
        })
    }

    /// Returns `true` if the terminal supports all advanced features.
    pub fn is_fully_featured(&self) -> bool {
        self.colour
            && self.cursor
            && self.bracketed_paste
            && self.focus_events
            && self.mouse
            && self.keyboard_enhancement
    }

    /// Returns `true` if the terminal supports basic TUI features.
    pub(crate) fn is_basic_tui(&self) -> bool {
        self.colour && self.cursor
    }
}

/// Check if the application is running in an interactive TTY context.
///
/// This is useful for deciding whether to use rich terminal features
/// or fall back to plain text output.
fn is_interactive_session() -> bool {
    io::stderr().is_tty() && io::stdin().is_tty()
}

/// Get the current terminal dimensions.
///
/// Returns `Some((width, height))` if the terminal size can be determined,
/// otherwise `None`.
pub fn terminal_size() -> Option<(u16, u16)> {
    crossterm::terminal::size().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_predicates_require_their_declared_features() {
        let fully_featured = TtyCapabilities {
            colour: true,
            cursor: true,
            bracketed_paste: true,
            focus_events: true,
            mouse: true,
            keyboard_enhancement: true,
        };
        assert!(fully_featured.is_fully_featured());
        assert!(fully_featured.is_basic_tui());

        let no_colour = TtyCapabilities { colour: false, ..fully_featured };
        assert!(!no_colour.is_fully_featured());
        assert!(!no_colour.is_basic_tui());

        let no_mouse = TtyCapabilities { mouse: false, ..fully_featured };
        assert!(!no_mouse.is_fully_featured());
        assert!(no_mouse.is_basic_tui());
    }
}

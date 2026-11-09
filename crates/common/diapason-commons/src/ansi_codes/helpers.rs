//! ANSI formatting and styled-output helper functions.

use super::styles::{
    BOLD, DIM, FG_BRIGHT_BLACK, FG_BRIGHT_CYAN, FG_BRIGHT_GREEN, FG_BRIGHT_RED, FG_BRIGHT_YELLOW, ITALIC, RESET,
    RESET_BOLD_DIM, RESET_ITALIC, RESET_UNDERLINE, UNDERLINE,
};
use super::{BEL, CSI, ESC, ESC_CHAR, OSC_HYPERLINK_PREFIX, OSC_SET_TITLE_PREFIX, ST};
use std::io::Write;

// === Helper Functions ===

/// Return a CSI sequence that moves the cursor up by `n` rows.
#[inline]
pub fn cursor_up(n: u16) -> String {
    format!("{CSI}{n}A")
}

#[inline]
/// Return a CSI sequence that moves the cursor down by `n` rows.
pub fn cursor_down(n: u16) -> String {
    format!("{CSI}{n}B")
}

#[inline]
/// Return a CSI sequence that moves the cursor right by `n` columns.
pub fn cursor_right(n: u16) -> String {
    format!("{CSI}{n}C")
}

#[inline]
/// Return a CSI sequence that moves the cursor left by `n` columns.
pub fn cursor_left(n: u16) -> String {
    format!("{CSI}{n}D")
}

#[inline]
/// Return a CSI sequence that moves the cursor to `row`;`col` (1-indexed).
pub fn cursor_to(row: u16, col: u16) -> String {
    format!("{CSI}{row};{col}H")
}

/// Build a portable in-place redraw prefix (`CR` + `EL2`).
///
/// This is the common CLI pattern for one-line progress updates.
#[cfg(test)]
const REDRAW_LINE_PREFIX: &str = "\r\x1b[2K";

#[cfg(test)]
#[inline]
fn redraw_line_prefix() -> &'static str {
    REDRAW_LINE_PREFIX
}

/// Format a one-line in-place update payload.
///
/// Equivalent to: `\\r\\x1b[2K{content}`.
#[cfg(test)]
#[inline]
fn format_redraw_line(content: &str) -> String {
    format!("{}{}", redraw_line_prefix(), content)
}

#[inline]
/// Return a sequence selecting the 256-colour palette entry for the foreground.
pub fn fg_256(colour_id: u8) -> String {
    format!("{CSI}38;5;{colour_id}m")
}

#[inline]
/// Return a sequence selecting the 256-colour palette entry for the background.
pub fn bg_256(colour_id: u8) -> String {
    format!("{CSI}48;5;{colour_id}m")
}

#[inline]
/// Return a sequence selecting a 24-bit RGB foreground colour.
pub fn fg_rgb(r: u8, g: u8, b: u8) -> String {
    format!("{CSI}38;2;{r};{g};{b}m")
}

#[inline]
/// Return a sequence selecting a 24-bit RGB background colour.
pub fn bg_rgb(r: u8, g: u8, b: u8) -> String {
    format!("{CSI}48;2;{r};{g};{b}m")
}

#[inline]
/// Wrap `text` in `colour` and reset the style afterwards.
pub fn coloured(text: &str, colour: &str) -> String {
    format!("{colour}{text}{RESET}")
}

#[inline]
/// Wrap `text` in bold.
pub fn bold(text: &str) -> String {
    format!("{BOLD}{text}{RESET_BOLD_DIM}")
}

#[inline]
/// Wrap `text` in italic.
pub fn italic(text: &str) -> String {
    format!("{ITALIC}{text}{RESET_ITALIC}")
}

#[inline]
/// Wrap `text` in underline.
pub fn underline(text: &str) -> String {
    format!("{UNDERLINE}{text}{RESET_UNDERLINE}")
}

#[inline]
/// Wrap `text` in dim.
pub fn dim(text: &str) -> String {
    format!("{DIM}{text}{RESET_BOLD_DIM}")
}

#[inline]
/// Apply `styles` in order, then `text`, then a reset.
pub fn combine_styles(text: &str, styles: &[&str]) -> String {
    let mut result = String::with_capacity(text.len() + styles.len() * 10);
    for style in styles {
        result.push_str(style);
    }
    result.push_str(text);
    result.push_str(RESET);
    result
}

/// Semantic style constants named by meaning rather than by colour.
pub mod semantic {
    use super::*;
    /// Style for error messages.
    pub const ERROR: &str = FG_BRIGHT_RED;
    /// Style for success messages.
    pub const SUCCESS: &str = FG_BRIGHT_GREEN;
    /// Style for warnings.
    pub const WARNING: &str = FG_BRIGHT_YELLOW;
    /// Style for informational messages.
    pub const INFO: &str = FG_BRIGHT_CYAN;
    /// Style for de-emphasized text.
    pub const MUTED: &str = DIM;
    /// Style for emphasized text.
    pub const EMPHASIS: &str = BOLD;
    /// Style for debug output.
    pub const DEBUG: &str = FG_BRIGHT_BLACK;
}

/// Return whether `text` contains an ANSI escape character.
#[inline]
#[must_use]
pub fn contains_ansi(text: &str) -> bool {
    text.contains(ESC_CHAR)
}

#[inline]
#[must_use]
/// Return whether `text` begins with an ANSI escape character.
pub fn starts_with_ansi(text: &str) -> bool {
    text.starts_with(ESC_CHAR)
}

#[inline]
#[must_use]
/// Return whether `text` ends with an SGR sequence.
pub fn ends_with_ansi(text: &str) -> bool {
    text.ends_with('m') && text.contains(ESC)
}

#[inline]
#[must_use]
/// Return the printable width of `text` once ANSI sequences are stripped.
pub fn display_width(text: &str) -> usize {
    crate::ansi::strip_ansi(text).len()
}

/// Right-pad `text` with `pad_char` until it reaches `width` printable columns.
pub fn pad_to_width(text: &str, width: usize, pad_char: char) -> String {
    let current_width = display_width(text);
    if current_width >= width {
        text.to_string()
    } else {
        let padding = pad_char.to_string().repeat(width - current_width);
        format!("{text}{padding}")
    }
}

/// Truncate `text` to `max_width` printable columns, appending `ellipsis`.
pub fn truncate_to_width(text: &str, max_width: usize, ellipsis: &str) -> String {
    let stripped = crate::ansi::strip_ansi(text);
    if stripped.len() <= max_width {
        return text.to_string();
    }

    let truncate_at = max_width.saturating_sub(ellipsis.len());
    let truncated_plain: String = stripped.chars().take(truncate_at).collect();

    if starts_with_ansi(text) {
        let mut ansi_prefix = String::new();
        for ch in text.chars() {
            ansi_prefix.push(ch);
            if ch == '\x1b' {
                continue;
            }
            if ch.is_alphabetic() && ansi_prefix.contains('\x1b') {
                break;
            }
        }
        format!("{ansi_prefix}{truncated_plain}{ellipsis}{RESET}")
    } else {
        format!("{truncated_plain}{ellipsis}")
    }
}

#[inline]
/// Write `text` to `writer` wrapped in `style`.
pub fn write_styled<W: Write>(writer: &mut W, text: &str, style: &str) -> std::io::Result<()> {
    writer.write_all(style.as_bytes())?;
    writer.write_all(text.as_bytes())?;
    writer.write_all(RESET.as_bytes())?;
    Ok(())
}

#[inline]
/// Append `text` wrapped in `style` to `buffer`.
pub fn format_styled_into(buffer: &mut String, text: &str, style: &str) {
    buffer.push_str(style);
    buffer.push_str(text);
    buffer.push_str(RESET);
}

/// Set scrolling region (DECSTBM) — top and bottom rows (1-indexed)
#[inline]
pub fn set_scroll_region(top: u16, bottom: u16) -> String {
    format!("{CSI}{top};{bottom}r")
}

/// Insert Ps lines at cursor position
#[inline]
pub fn insert_lines(n: u16) -> String {
    format!("{CSI}{n}L")
}

/// Delete Ps lines at cursor position
#[inline]
pub fn delete_lines(n: u16) -> String {
    format!("{CSI}{n}M")
}

/// Scroll up Ps lines
#[inline]
pub fn scroll_up(n: u16) -> String {
    format!("{CSI}{n}S")
}

/// Scroll down Ps lines
#[inline]
pub fn scroll_down(n: u16) -> String {
    format!("{CSI}{n}T")
}

/// Build an OSC sequence to set the terminal window title
#[inline]
pub fn set_window_title(title: &str) -> String {
    format!("{OSC_SET_TITLE_PREFIX}{title}{BEL}")
}

/// Build an OSC 8 hyperlink open sequence
#[inline]
pub fn hyperlink_open(url: &str) -> String {
    format!("{OSC_HYPERLINK_PREFIX};{url}{ST}")
}

/// Build an OSC 8 hyperlink close sequence
#[inline]
pub fn hyperlink_close() -> String {
    format!("{OSC_HYPERLINK_PREFIX};{ST}")
}

#[cfg(test)]
#[path = "helpers_tests.rs"]
mod tests;

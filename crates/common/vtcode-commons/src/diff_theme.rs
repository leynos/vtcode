//! Diff theme configuration and color palettes
//!
//! Uses subtle red/green tints for diff line backgrounds.

use anstyle::{AnsiColor, Color};

use crate::ansi_capabilities::{ColourScheme, detect_colour_scheme};

/// Terminal background theme for diff rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffTheme {
    Dark,
    Light,
}

impl DiffTheme {
    /// Detect theme from the terminal environment.
    pub fn detect() -> Self {
        match detect_colour_scheme() {
            ColourScheme::Light => Self::Light,
            ColourScheme::Dark | ColourScheme::Unknown => Self::Dark,
        }
    }

    pub fn is_light(self) -> bool {
        self == Self::Light
    }
}

/// Terminal color capability level for palette selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffColourLevel {
    TrueColour,
    Ansi256,
    Ansi16,
}

impl DiffColourLevel {
    /// Detect color level from terminal capabilities.
    pub fn detect() -> Self {
        let colourterm = std::env::var("COLORTERM").unwrap_or_default();
        let term = std::env::var("TERM").unwrap_or_default();
        let term_program = std::env::var("TERM_PROGRAM").ok();
        let has_wt_session = std::env::var_os("WT_SESSION").is_some();
        let has_force_colour_override = std::env::var_os("FORCE_COLOR").is_some();

        diff_colour_level_for_terminal(
            base_diff_colour_level(&colourterm, &term),
            term_program.as_deref(),
            has_wt_session,
            has_force_colour_override,
        )
    }
}

fn base_diff_colour_level(colourterm: &str, term: &str) -> DiffColourLevel {
    let colourterm = colourterm.to_ascii_lowercase();
    let term = term.to_ascii_lowercase();

    if colourterm.contains("truecolor") || colourterm.contains("24bit") {
        DiffColourLevel::TrueColour
    } else if term.contains("256") {
        DiffColourLevel::Ansi256
    } else {
        DiffColourLevel::Ansi16
    }
}

fn diff_colour_level_for_terminal(
    base_level: DiffColourLevel,
    term_program: Option<&str>,
    has_wt_session: bool,
    has_force_colour_override: bool,
) -> DiffColourLevel {
    if has_force_colour_override {
        return base_level;
    }

    if has_wt_session || (base_level == DiffColourLevel::Ansi16 && is_windows_terminal(term_program)) {
        return DiffColourLevel::TrueColour;
    }

    base_level
}

fn is_windows_terminal(term_program: Option<&str>) -> bool {
    let Some(program) = term_program else {
        return false;
    };

    let normalized = program.trim().to_ascii_lowercase();
    normalized.contains("windows_terminal") || normalized.contains("windows terminal")
}

// ── Standard ANSI red/green backgrounds ────────────────────────────────────

/// Get background color for addition lines based on theme and color level.
pub fn diff_add_bg(theme: DiffTheme, _level: DiffColourLevel) -> Color {
    match theme {
        DiffTheme::Dark => Color::Rgb(anstyle::RgbColor(20, 58, 45)),
        DiffTheme::Light => Color::Rgb(anstyle::RgbColor(218, 246, 225)),
    }
}

/// Get background color for deletion lines based on theme and color level.
pub fn diff_del_bg(theme: DiffTheme, _level: DiffColourLevel) -> Color {
    match theme {
        DiffTheme::Dark => Color::Rgb(anstyle::RgbColor(70, 38, 42)),
        DiffTheme::Light => Color::Rgb(anstyle::RgbColor(255, 224, 224)),
    }
}

/// Get gutter foreground color for light theme (dark theme uses dimmed default).
pub fn diff_gutter_fg_light(_level: DiffColourLevel) -> Color {
    Color::Ansi(AnsiColor::Black)
}

/// Get gutter background color for addition lines in light theme.
pub fn diff_gutter_bg_add_light(_level: DiffColourLevel) -> Color {
    Color::Ansi(AnsiColor::BrightGreen)
}

/// Get gutter background color for deletion lines in light theme.
pub fn diff_gutter_bg_del_light(_level: DiffColourLevel) -> Color {
    Color::Ansi(AnsiColor::BrightRed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_add_bg_is_subtle_green_tint() {
        let bg = diff_add_bg(DiffTheme::Dark, DiffColourLevel::TrueColour);
        assert_eq!(bg, Color::Rgb(anstyle::RgbColor(20, 58, 45)));
    }

    #[test]
    fn dark_del_bg_is_subtle_red_tint() {
        let bg = diff_del_bg(DiffTheme::Dark, DiffColourLevel::TrueColour);
        assert_eq!(bg, Color::Rgb(anstyle::RgbColor(70, 38, 42)));
    }

    #[test]
    fn light_add_bg_is_subtle_green_tint() {
        let bg = diff_add_bg(DiffTheme::Light, DiffColourLevel::TrueColour);
        assert_eq!(bg, Color::Rgb(anstyle::RgbColor(218, 246, 225)));
    }

    #[test]
    fn light_del_bg_is_subtle_red_tint() {
        let bg = diff_del_bg(DiffTheme::Light, DiffColourLevel::TrueColour);
        assert_eq!(bg, Color::Rgb(anstyle::RgbColor(255, 224, 224)));
    }

    #[test]
    fn all_levels_use_same_theme_tints() {
        for level in [
            DiffColourLevel::TrueColour,
            DiffColourLevel::Ansi256,
            DiffColourLevel::Ansi16,
        ] {
            assert_eq!(diff_add_bg(DiffTheme::Dark, level), Color::Rgb(anstyle::RgbColor(20, 58, 45)));
            assert_eq!(diff_del_bg(DiffTheme::Dark, level), Color::Rgb(anstyle::RgbColor(70, 38, 42)));
            assert_eq!(diff_add_bg(DiffTheme::Light, level), Color::Rgb(anstyle::RgbColor(218, 246, 225)));
            assert_eq!(diff_del_bg(DiffTheme::Light, level), Color::Rgb(anstyle::RgbColor(255, 224, 224)));
        }
    }

    #[test]
    fn wt_session_promotes_ansi16_to_truecolour() {
        assert_eq!(
            diff_colour_level_for_terminal(DiffColourLevel::Ansi16, None, true, false),
            DiffColourLevel::TrueColour
        );
    }

    #[test]
    fn windows_terminal_term_program_promotes_ansi16_to_truecolour() {
        assert_eq!(
            diff_colour_level_for_terminal(DiffColourLevel::Ansi16, Some("Windows_Terminal"), false, false),
            DiffColourLevel::TrueColour
        );
    }

    #[test]
    fn non_windows_terminal_keeps_ansi16() {
        assert_eq!(
            diff_colour_level_for_terminal(DiffColourLevel::Ansi16, Some("WezTerm"), false, false),
            DiffColourLevel::Ansi16
        );
    }

    #[test]
    fn force_colour_keeps_ansi16_when_wt_session_exists() {
        assert_eq!(diff_colour_level_for_terminal(DiffColourLevel::Ansi16, None, true, true), DiffColourLevel::Ansi16);
    }

    #[test]
    fn force_colour_keeps_ansi256_when_wt_session_exists() {
        assert_eq!(
            diff_colour_level_for_terminal(DiffColourLevel::Ansi256, None, true, true),
            DiffColourLevel::Ansi256
        );
    }

    #[test]
    fn base_level_detects_truecolour_from_colourterm() {
        assert_eq!(base_diff_colour_level("truecolor", "xterm-256color"), DiffColourLevel::TrueColour);
    }

    #[test]
    fn base_level_detects_ansi256_from_term() {
        assert_eq!(base_diff_colour_level("", "xterm-256color"), DiffColourLevel::Ansi256);
    }

    #[test]
    fn base_level_falls_back_to_ansi16() {
        assert_eq!(base_diff_colour_level("", "xterm"), DiffColourLevel::Ansi16);
    }
}

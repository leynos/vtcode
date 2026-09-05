use anstyle::{Color as AnsiColourEnum, Style as AnsiStyle};
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::tui::ui::theme;

// Re-export from commons so existing consumers don't break.
pub use vtcode_commons::ui_protocol::{convert_style, theme_from_colour_fields};

use super::types::{InlineTextStyle, InlineTheme};

pub fn theme_from_styles(styles: &theme::ThemeStyles) -> InlineTheme {
    theme_from_colour_fields(
        styles.foreground,
        styles.background,
        styles.primary,
        styles.secondary,
        styles.tool,
        styles.tool_detail,
        styles.pty_output,
    )
}

pub(crate) fn measure_text_width(text: &str) -> u16 {
    UnicodeWidthStr::width(text) as u16
}

/// Convert anstyle Color to ratatui Color.
///
/// Delegates to `crate::design::colour::anstyle_to_ratatui_colour` which
/// provides the correct mapping (fixing the Magenta bug).
pub(crate) fn ratatui_colour_from_ansi(colour: AnsiColourEnum) -> Color {
    crate::design::colour::anstyle_to_ratatui_colour(colour)
}

/// Parse a hex colour string (e.g., "#D99A4E") to a ratatui Color.
/// Returns None if the string is invalid or cannot be parsed.
pub(crate) use crate::design::colour::hex_to_ratatui_colour;

/// Get the agent colour style from an optional colour token.
///
/// The token may be a primary-agent mode name (`"build"`), a standard ANSI hue
/// name (`"green"`), or a `#rrggbb` hex string. It is resolved theme-aware via
/// the design system so the badge stays legible on both dark and light
/// terminals, with `fallback_colour` used when the token is empty or unknown.
pub(crate) fn agent_colour_style(colour: Option<&str>, fallback_colour: Color) -> Style {
    let light = matches!(
        vtcode_commons::ansi_capabilities::detect_colour_scheme(),
        vtcode_commons::ansi_capabilities::ColourScheme::Light
    );
    let colour = colour
        .map(|c| crate::design::colour::resolve_agent_colour(c, fallback_colour, light))
        .unwrap_or(fallback_colour);
    Style::default().fg(colour).add_modifier(Modifier::BOLD)
}

pub(crate) fn ratatui_style_from_inline(style: &InlineTextStyle, fallback: Option<AnsiColourEnum>) -> Style {
    crate::design::style::inline_text_style_to_ratatui(style.colour, style.bg_colour, style.effects, fallback)
}

/// PTY output style helper: keep configured colours, suppress bold, and enforce dimmed output.
pub(crate) fn ratatui_pty_style_from_inline(style: &InlineTextStyle, fallback: Option<AnsiColourEnum>) -> Style {
    ratatui_style_from_inline(style, fallback)
        .remove_modifier(Modifier::BOLD)
        .add_modifier(Modifier::DIM)
}

const PTY_DETAIL_COLOUR_BACKGROUND_MIX: f32 = 0.35;

/// PTY detail style helper: attenuate explicit ANSI colours toward the background
/// before applying the standard subdued PTY output treatment.
pub(crate) fn ratatui_pty_detail_style_from_inline(
    style: &InlineTextStyle,
    fallback: Option<AnsiColourEnum>,
    background: Option<AnsiColourEnum>,
) -> Style {
    let mut detail_style = style.clone();
    if let (Some(colour), Some(background)) = (detail_style.colour, background)
        && let Some(dimmed) =
            vtcode_commons::colours::blend_colours(&colour, &background, PTY_DETAIL_COLOUR_BACKGROUND_MIX)
    {
        detail_style.colour = Some(dimmed);
    }

    ratatui_pty_style_from_inline(&detail_style, fallback)
}

/// Convert an `anstyle::Style` directly to a `ratatui::style::Style`.
pub(crate) fn ratatui_style_from_ansi(style: AnsiStyle) -> Style {
    crate::design::style::anstyle_to_ratatui_style(style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::colour::resolve_agent_colour;
    use vtcode_config::constants::ui::{AGENT_COLOUR_AUTO, AGENT_COLOUR_BUILD, AGENT_COLOUR_DUCK, AGENT_COLOUR_PLAN};

    #[test]
    fn agent_colour_style_applies_mode_colour_with_bold() {
        let fallback = Color::LightMagenta;
        let style = agent_colour_style(Some(AGENT_COLOUR_BUILD), fallback);
        // The exact variant depends on the detected terminal scheme, but it must
        // be a concrete (non-fallback) standard colour and always bold.
        assert_ne!(style.fg, Some(fallback));
        assert!(style.add_modifier.contains(Modifier::BOLD));

        for hue in [
            AGENT_COLOUR_BUILD,
            AGENT_COLOUR_AUTO,
            AGENT_COLOUR_PLAN,
            AGENT_COLOUR_DUCK,
        ] {
            let s = agent_colour_style(Some(hue), fallback);
            assert!(s.add_modifier.contains(Modifier::BOLD));
            assert!(s.fg.is_some());
        }
    }

    #[test]
    fn agent_colour_style_is_theme_aware_and_distinct_per_mode() {
        let fallback = Color::LightMagenta;
        // On a dark terminal each mode resolves to its bright variant.
        let dark = [
            resolve_agent_colour(AGENT_COLOUR_BUILD, fallback, false),
            resolve_agent_colour(AGENT_COLOUR_AUTO, fallback, false),
            resolve_agent_colour(AGENT_COLOUR_PLAN, fallback, false),
            resolve_agent_colour(AGENT_COLOUR_DUCK, fallback, false),
        ];
        assert_eq!(
            dark,
            [
                Color::LightRed,
                Color::LightGreen,
                Color::LightBlue,
                Color::LightMagenta
            ]
        );
        // On a light terminal each mode resolves to its base variant.
        let light = [
            resolve_agent_colour(AGENT_COLOUR_BUILD, fallback, true),
            resolve_agent_colour(AGENT_COLOUR_AUTO, fallback, true),
            resolve_agent_colour(AGENT_COLOUR_PLAN, fallback, true),
            resolve_agent_colour(AGENT_COLOUR_DUCK, fallback, true),
        ];
        assert_eq!(light, [Color::Red, Color::Green, Color::Blue, Color::Magenta]);
        // The four modes must remain visually distinct in both appearances.
        assert_eq!(dark.iter().collect::<std::collections::HashSet<_>>().len(), 4);
        assert_eq!(light.iter().collect::<std::collections::HashSet<_>>().len(), 4);
    }

    #[test]
    fn agent_colour_style_accepts_raw_hue_names_and_hex() {
        let fallback = Color::LightMagenta;
        // Raw standard ANSI hue name (as emitted by the plan-approval overlay).
        assert_eq!(resolve_agent_colour("green", fallback, false), Color::LightGreen);
        assert_eq!(resolve_agent_colour("blue", fallback, true), Color::Blue);
        // Legacy hex still resolves.
        assert_eq!(resolve_agent_colour("#FF0000", fallback, false), Color::Rgb(255, 0, 0));
    }

    #[test]
    fn agent_colour_style_falls_back_when_missing_or_invalid() {
        let fallback = Color::LightMagenta;
        let missing = agent_colour_style(None, fallback);
        assert_eq!(missing.fg, Some(fallback));
        assert!(missing.add_modifier.contains(Modifier::BOLD));

        let invalid = agent_colour_style(Some("not-a-colour"), fallback);
        assert_eq!(invalid.fg, Some(fallback));
    }
}

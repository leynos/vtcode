use vtcode_commons::ansi_capabilities::{ColourScheme, detect_colour_scheme};

use crate::theme::colour_math::relative_luminance;
use crate::theme::registry::theme_definition;
use crate::theme::types::DEFAULT_THEME_ID;

/// Report whether a theme matches the detected terminal light/dark scheme.
pub fn theme_matches_terminal_scheme(theme_id: &str) -> bool {
    let scheme = detect_colour_scheme();
    let theme_is_light = is_light_theme(theme_id);

    match scheme {
        ColourScheme::Light => theme_is_light,
        ColourScheme::Dark | ColourScheme::Unknown => !theme_is_light,
    }
}

/// Report whether a built-in theme should be treated as a light theme.
pub fn is_light_theme(theme_id: &str) -> bool {
    theme_definition(theme_id)
        .map(|theme| relative_luminance(theme.palette.background) > 0.5)
        .unwrap_or(false)
}

/// Suggest a built-in theme that matches the current terminal scheme.
pub fn suggest_theme_for_terminal() -> &'static str {
    match detect_colour_scheme() {
        ColourScheme::Light => "vitesse-light",
        ColourScheme::Dark | ColourScheme::Unknown => DEFAULT_THEME_ID,
    }
}

use anstyle::{Color, Effects, RgbColor, Style};
use vtcode_config::constants::{defaults, ui};

use crate::theme::colour_math::{balance_text_luminance, ensure_contrast, lighten, mix};

/// Identifier for the default theme.
pub const DEFAULT_THEME_ID: &str = defaults::DEFAULT_THEME;

const DEFAULT_MIN_CONTRAST: f32 = ui::THEME_MIN_CONTRAST_RATIO;

/// Color accessibility configuration loaded from vtcode.toml.
#[derive(Clone, Debug)]
pub struct ColourAccessibilityConfig {
    pub minimum_contrast: f32,
    pub bold_is_bright: bool,
    pub safe_colours_only: bool,
}

impl Default for ColourAccessibilityConfig {
    fn default() -> Self {
        Self {
            minimum_contrast: DEFAULT_MIN_CONTRAST,
            bold_is_bright: false,
            safe_colours_only: false,
        }
    }
}

/// Palette describing UI colours for the terminal experience.
#[derive(Clone, Debug)]
pub struct ThemePalette {
    pub(crate) primary_accent: RgbColor,
    pub(crate) background: RgbColor,
    pub(crate) foreground: RgbColor,
    pub(crate) secondary_accent: RgbColor,
    pub(crate) alert: RgbColor,
    pub(crate) logo_accent: RgbColor,
}

/// Shared computation context for theme color derivation.
///
/// Holds invariant parameters (background, min_contrast) that every color
/// computation needs, eliminating repetitive argument passing across the
/// 14+ color derivations in the theme pipeline.
#[derive(Clone, Debug)]
pub(crate) struct ColourContext {
    pub background: RgbColor,
    pub min_contrast: f32,
    pub fallback_light: RgbColor,
}

impl ColourContext {
    fn new(background: RgbColor, min_contrast: f32) -> Self {
        Self {
            background,
            min_contrast,
            fallback_light: RgbColor(
                ui::THEME_COLOUR_WHITE_RED,
                ui::THEME_COLOUR_WHITE_GREEN,
                ui::THEME_COLOUR_WHITE_BLUE,
            ),
        }
    }

    /// Ensure minimum contrast against background, then balance luminance
    /// into the comfortable reading range. Used for text-content colours.
    fn guaranteed_text_colour(&self, candidate: RgbColor, fallbacks: &[RgbColor]) -> RgbColor {
        let colour = ensure_contrast(candidate, self.background, self.min_contrast, fallbacks);
        balance_text_luminance(colour, self.background, self.min_contrast)
    }

    /// Ensure minimum contrast against background only. Used for accent/UI
    /// colours where luminance balancing would override the intended tint.
    fn guaranteed_accent_colour(&self, candidate: RgbColor, fallbacks: &[RgbColor]) -> RgbColor {
        ensure_contrast(candidate, self.background, self.min_contrast, fallbacks)
    }

    /// 1. Main foreground text color.
    fn compute_text_colour(&self, foreground: RgbColor, secondary: RgbColor) -> RgbColor {
        self.guaranteed_text_colour(
            foreground,
            &[
                lighten(foreground, ui::THEME_FOREGROUND_LIGHTEN_RATIO),
                lighten(secondary, ui::THEME_SECONDARY_LIGHTEN_RATIO),
                self.fallback_light,
            ],
        )
    }

    /// 2. Info/muted text color (secondary accent adapted for readability).
    fn compute_info_colour(&self, secondary: RgbColor, text_colour: RgbColor) -> RgbColor {
        self.guaranteed_text_colour(
            secondary,
            &[
                lighten(secondary, ui::THEME_SECONDARY_LIGHTEN_RATIO),
                text_colour,
                self.fallback_light,
            ],
        )
    }

    /// 3. Tool accent color (text_colour lightened and contrast-ensured).
    fn compute_tool_colour(&self, text_colour: RgbColor) -> RgbColor {
        self.guaranteed_accent_colour(
            lighten(text_colour, ui::THEME_MIX_RATIO),
            &[
                lighten(lighten(text_colour, ui::THEME_MIX_RATIO), ui::THEME_TOOL_BODY_LIGHTEN_RATIO),
                text_colour,
                self.fallback_light,
            ],
        )
    }

    /// 4. Tool body text color (subdued variant of tool accent).
    fn compute_tool_body_colour(&self, text_colour: RgbColor) -> RgbColor {
        let candidate = mix(lighten(text_colour, ui::THEME_MIX_RATIO), text_colour, ui::THEME_TOOL_BODY_MIX_RATIO);
        self.guaranteed_accent_colour(
            candidate,
            &[
                lighten(lighten(text_colour, ui::THEME_MIX_RATIO), ui::THEME_TOOL_BODY_LIGHTEN_RATIO),
                text_colour,
                self.fallback_light,
            ],
        )
    }

    /// 5. PTY/shell output color — dimmed by blending tool_body toward the
    ///    background, then balanced for readability.
    fn compute_pty_output_colour(&self, tool_body_colour: RgbColor, text_colour: RgbColor) -> RgbColor {
        let candidate = mix(tool_body_colour, self.background, ui::THEME_PTY_OUTPUT_MIX_RATIO);
        self.guaranteed_text_colour(candidate, &[tool_body_colour, text_colour])
    }

    /// 6. Response/assistant text color.
    fn compute_response_colour(&self, text_colour: RgbColor) -> RgbColor {
        self.guaranteed_text_colour(
            text_colour,
            &[
                lighten(text_colour, ui::THEME_RESPONSE_COLOUR_LIGHTEN_RATIO),
                self.fallback_light,
            ],
        )
    }

    /// 7. Reasoning text color (lightened text, DIMMED+ITALIC applied separately).
    fn compute_reasoning_colour(&self, text_colour: RgbColor) -> RgbColor {
        self.guaranteed_text_colour(
            lighten(text_colour, 0.25),
            &[lighten(text_colour, 0.15), text_colour, self.fallback_light],
        )
    }

    /// 8. User input text color.
    fn compute_user_colour(&self, secondary: RgbColor, info_colour: RgbColor, text_colour: RgbColor) -> RgbColor {
        self.guaranteed_text_colour(
            lighten(secondary, ui::THEME_USER_COLOUR_LIGHTEN_RATIO),
            &[
                lighten(secondary, ui::THEME_SECONDARY_USER_COLOUR_LIGHTEN_RATIO),
                info_colour,
                text_colour,
            ],
        )
    }

    /// 9. Alert/error color.
    fn compute_alert_colour(&self, alert: RgbColor, text_colour: RgbColor) -> RgbColor {
        self.guaranteed_text_colour(
            alert,
            &[
                lighten(alert, ui::THEME_LUMINANCE_LIGHTEN_RATIO),
                self.fallback_light,
                text_colour,
            ],
        )
    }

    /// 10. Primary accent (for UI chrome, not body text).
    fn compute_primary_colour(&self, primary: RgbColor, text_colour: RgbColor) -> RgbColor {
        self.guaranteed_text_colour(
            ensure_contrast(primary, self.background, self.min_contrast, &[text_colour]),
            &[text_colour],
        )
    }

    /// 11. Secondary accent (for UI chrome).
    fn compute_secondary_colour(&self, secondary: RgbColor, info_colour: RgbColor, text_colour: RgbColor) -> RgbColor {
        self.guaranteed_text_colour(
            ensure_contrast(secondary, self.background, self.min_contrast, &[info_colour, text_colour]),
            &[info_colour, text_colour],
        )
    }

    /// 12. Logo accent color.
    fn compute_logo_colour(
        &self,
        logo_accent: RgbColor,
        secondary_colour: RgbColor,
        text_colour: RgbColor,
    ) -> RgbColor {
        self.guaranteed_text_colour(
            ensure_contrast(logo_accent, self.background, self.min_contrast, &[secondary_colour, text_colour]),
            &[secondary_colour, text_colour],
        )
    }

    /// 13. Status banner color (lightened primary).
    fn compute_status_colour(
        &self,
        primary_colour: RgbColor,
        info_colour: RgbColor,
        text_colour: RgbColor,
    ) -> RgbColor {
        self.guaranteed_accent_colour(
            lighten(primary_colour, ui::THEME_PRIMARY_STATUS_LIGHTEN_RATIO),
            &[
                lighten(primary_colour, ui::THEME_PRIMARY_STATUS_SECONDARY_LIGHTEN_RATIO),
                info_colour,
                text_colour,
            ],
        )
    }

    /// 14. MCP badge color (lightened logo accent).
    fn compute_mcp_colour(&self, logo_colour: RgbColor, info_colour: RgbColor) -> RgbColor {
        self.guaranteed_accent_colour(
            lighten(logo_colour, ui::THEME_SECONDARY_LIGHTEN_RATIO),
            &[
                lighten(logo_colour, ui::THEME_LOGO_ACCENT_BANNER_LIGHTEN_RATIO),
                info_colour,
                self.fallback_light,
            ],
        )
    }
}

impl ThemePalette {
    fn style_from(colour: RgbColor, bold: bool, bold_is_bright: bool) -> Style {
        let mut style = Style::new().fg_color(Some(Color::Rgb(colour)));
        if bold && !bold_is_bright {
            style = style.bold();
        }
        style
    }

    pub(crate) fn build_styles_with_accessibility(&self, accessibility: &ColourAccessibilityConfig) -> ThemeStyles {
        let ctx = ColourContext::new(self.background, accessibility.minimum_contrast);
        let bold_is_bright = accessibility.bold_is_bright;

        let text = ctx.compute_text_colour(self.foreground, self.secondary_accent);
        let info = ctx.compute_info_colour(self.secondary_accent, text);
        let tool_body = ctx.compute_tool_body_colour(text);
        let pty = ctx.compute_pty_output_colour(tool_body, text);
        let primary = ctx.compute_primary_colour(self.primary_accent, text);
        let secondary = ctx.compute_secondary_colour(self.secondary_accent, info, text);
        let logo = ctx.compute_logo_colour(self.logo_accent, secondary, text);

        ThemeStyles {
            info: Self::style_from(info, true, bold_is_bright),
            error: Self::style_from(ctx.compute_alert_colour(self.alert, text), true, bold_is_bright),
            output: Self::style_from(text, false, bold_is_bright),
            response: Self::style_from(ctx.compute_response_colour(text), false, bold_is_bright),
            reasoning: Self::style_from(ctx.compute_reasoning_colour(text), false, bold_is_bright)
                .effects(Effects::DIMMED | Effects::ITALIC),
            tool: Style::new().fg_color(Some(Color::Rgb(ctx.compute_tool_colour(text)))),
            tool_detail: Style::new().fg_color(Some(Color::Rgb(tool_body))),
            tool_output: Style::new(),
            pty_output: Style::new().fg_color(Some(Color::Rgb(pty))),
            status: Self::style_from(ctx.compute_status_colour(primary, info, text), true, bold_is_bright),
            mcp: Self::style_from(ctx.compute_mcp_colour(logo, info), true, bold_is_bright),
            user: Self::style_from(ctx.compute_user_colour(self.secondary_accent, info, text), false, bold_is_bright),
            primary: Self::style_from(primary, false, bold_is_bright),
            secondary: Self::style_from(secondary, false, bold_is_bright),
            background: Color::Rgb(self.background),
            foreground: Color::Rgb(text),
        }
    }
}

/// Styles computed from palette colours.
#[derive(Clone, Debug)]
pub struct ThemeStyles {
    pub info: Style,
    pub error: Style,
    pub output: Style,
    pub response: Style,
    pub reasoning: Style,
    pub tool: Style,
    pub tool_detail: Style,
    pub tool_output: Style,
    pub pty_output: Style,
    pub status: Style,
    pub mcp: Style,
    pub user: Style,
    pub primary: Style,
    pub secondary: Style,
    pub background: Color,
    pub foreground: Color,
}

#[derive(Clone, Debug)]
pub struct ThemeDefinition {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) palette: ThemePalette,
}

/// Logical grouping of built-in themes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeSuite {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) theme_ids: Vec<&'static str>,
}

/// Theme validation result.
#[derive(Debug, Clone)]
pub struct ThemeValidationResult {
    pub(crate) is_valid: bool,
    pub warnings: Vec<String>,
    pub(crate) errors: Vec<String>,
}

//! Style and theming types that depend on `anstyle`.

use std::sync::Arc;

use anstyle::{Color as AnsiColourEnum, Effects, Style as AnsiStyle};

/// Inline text styling with foreground/background color and text effects.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InlineTextStyle {
    pub colour: Option<AnsiColourEnum>,
    pub bg_colour: Option<AnsiColourEnum>,
    pub effects: Effects,
}

impl InlineTextStyle {
    #[must_use]
    pub fn with_colour(mut self, colour: Option<AnsiColourEnum>) -> Self {
        self.colour = colour;
        self
    }

    #[must_use]
    pub fn with_bg_colour(mut self, colour: Option<AnsiColourEnum>) -> Self {
        self.bg_colour = colour;
        self
    }

    #[must_use]
    pub fn merge_colour(mut self, fallback: Option<AnsiColourEnum>) -> Self {
        if self.colour.is_none() {
            self.colour = fallback;
        }
        self
    }

    #[must_use]
    pub fn merge_bg_colour(mut self, fallback: Option<AnsiColourEnum>) -> Self {
        if self.bg_colour.is_none() {
            self.bg_colour = fallback;
        }
        self
    }

    #[must_use]
    pub fn bold(mut self) -> Self {
        self.effects |= Effects::BOLD;
        self
    }

    #[must_use]
    pub fn italic(mut self) -> Self {
        self.effects |= Effects::ITALIC;
        self
    }

    #[must_use]
    pub fn underline(mut self) -> Self {
        self.effects |= Effects::UNDERLINE;
        self
    }

    #[must_use]
    pub fn dim(mut self) -> Self {
        self.effects |= Effects::DIMMED;
        self
    }

    #[must_use]
    pub fn to_ansi_style(&self, fallback: Option<AnsiColourEnum>) -> AnsiStyle {
        let mut style = AnsiStyle::new();
        if let Some(colour) = self.colour.or(fallback) {
            style = style.fg_color(Some(colour));
        }
        if let Some(bg) = self.bg_colour {
            style = style.bg_color(Some(bg));
        }
        if self.effects.contains(Effects::BOLD) {
            style = style.bold();
        }
        if self.effects.contains(Effects::ITALIC) {
            style = style.italic();
        }
        if self.effects.contains(Effects::UNDERLINE) {
            style = style.underline();
        }
        if self.effects.contains(Effects::DIMMED) {
            style = style.dimmed();
        }
        style
    }
}

/// A styled text segment with shared style.
#[derive(Clone, Debug, Default)]
pub struct InlineSegment {
    pub text: String,
    pub style: Arc<InlineTextStyle>,
}

/// A clickable link target inside a transcript line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineLinkTarget {
    Url(String),
}

/// Byte-range inside a line that is a clickable link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineLinkRange {
    pub start: usize,
    pub end: usize,
    pub target: InlineLinkTarget,
}

/// Resolved theme colours for inline rendering.
#[derive(Clone, Debug, Default)]
pub struct InlineTheme {
    pub foreground: Option<AnsiColourEnum>,
    pub background: Option<AnsiColourEnum>,
    pub primary: Option<AnsiColourEnum>,
    pub secondary: Option<AnsiColourEnum>,
    pub tool_accent: Option<AnsiColourEnum>,
    pub tool_body: Option<AnsiColourEnum>,
    pub pty_body: Option<AnsiColourEnum>,
}

// ---------------------------------------------------------------------------
// Header context types
// ---------------------------------------------------------------------------

/// Status-badge tone used in header status indicators.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InlineHeaderStatusTone {
    #[default]
    Ready,
    Warning,
    Error,
}

/// A labelled status badge for the header bar.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlineHeaderStatusBadge {
    pub text: String,
    pub tone: InlineHeaderStatusTone,
}

/// A compact pill badge rendered in the header.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InlineHeaderBadge {
    pub text: String,
    pub style: InlineTextStyle,
    pub full_background: bool,
}

/// A title + content highlight block in the header.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlineHeaderHighlight {
    pub title: String,
    pub lines: Vec<String>,
}

/// Session metadata displayed in the inline header.
#[derive(Clone, Debug)]
pub struct InlineHeaderContext {
    pub app_name: String,
    pub provider: String,
    pub model: String,
    pub context_window_size: Option<usize>,
    pub version: String,
    pub search_tools: Option<InlineHeaderStatusBadge>,
    pub persistent_memory: Option<InlineHeaderStatusBadge>,
    pub pr_review: Option<InlineHeaderStatusBadge>,
    pub editor_context: Option<String>,
    pub git: String,
    pub reasoning: String,
    pub reasoning_stage: Option<String>,
    pub workspace_trust: String,
    pub tools: String,
    pub mcp: String,
    pub primary_agent: Option<String>,
    pub primary_agent_colour: Option<String>,
    pub highlights: Vec<InlineHeaderHighlight>,
    pub subagent_badges: Vec<InlineHeaderBadge>,
}

impl Default for InlineHeaderContext {
    fn default() -> Self {
        let version = env!("CARGO_PKG_VERSION").to_string();
        Self {
            app_name: "App".to_string(),
            provider: "Provider: unavailable".to_string(),
            model: "Model: unavailable".to_string(),
            context_window_size: None,
            version,
            search_tools: None,
            persistent_memory: None,
            pr_review: None,
            editor_context: None,
            git: "git: unavailable".to_string(),
            reasoning: "unavailable".to_string(),
            reasoning_stage: None,
            workspace_trust: "Trust: unavailable".to_string(),
            tools: "Tools: unavailable".to_string(),
            mcp: "MCP: unavailable".to_string(),
            primary_agent: None,
            primary_agent_colour: None,
            highlights: Vec::new(),
            subagent_badges: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn convert_ansi_colour(colour: AnsiColourEnum) -> Option<AnsiColourEnum> {
    Some(match colour {
        AnsiColourEnum::Ansi(ansi) => AnsiColourEnum::Ansi(ansi),
        AnsiColourEnum::Ansi256(value) => AnsiColourEnum::Ansi256(value),
        AnsiColourEnum::Rgb(rgb) => AnsiColourEnum::Rgb(rgb),
    })
}

fn convert_style_colour(style: &AnsiStyle) -> Option<AnsiColourEnum> {
    style.get_fg_color().and_then(convert_ansi_colour)
}

fn convert_style_bg_colour(style: &AnsiStyle) -> Option<AnsiColourEnum> {
    style.get_bg_color().and_then(convert_ansi_colour)
}

/// Convert an `anstyle::Style` to an [`InlineTextStyle`].
pub fn convert_style(style: AnsiStyle) -> InlineTextStyle {
    InlineTextStyle {
        colour: convert_style_colour(&style),
        bg_colour: convert_style_bg_colour(&style),
        effects: style.get_effects(),
    }
}

/// Build an [`InlineTheme`] from individual theme colour fields.
pub fn theme_from_colour_fields(
    foreground: AnsiColourEnum,
    background: AnsiColourEnum,
    primary: AnsiStyle,
    secondary: AnsiStyle,
    tool: AnsiStyle,
    tool_detail: AnsiStyle,
    pty_output: AnsiStyle,
) -> InlineTheme {
    InlineTheme {
        foreground: convert_ansi_colour(foreground),
        background: convert_ansi_colour(background),
        primary: convert_style_colour(&primary),
        secondary: convert_style_colour(&secondary),
        tool_accent: convert_style_colour(&tool),
        tool_body: convert_style_colour(&tool_detail),
        pty_body: convert_style_colour(&pty_output),
    }
}

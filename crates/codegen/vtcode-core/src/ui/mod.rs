//! User interface utilities and shared UI components
//!
//! This module contains shared UI functionality including loading indicators,
//! markdown rendering, and terminal utilities.

/// Unified diff rendering with ANSI styling and suppression logic.
pub mod diff_renderer;
/// Git color configuration parsing.
pub mod git_config;
/// Markdown-to-ANSI rendering for chat output.
pub mod markdown;
/// Fuzzy search utilities.
pub mod search;
/// Slash command discovery and suggestion.
pub mod slash;
/// Streaming text buffer for incremental output.
pub mod stream_buffer;
/// Styled text helpers.
pub mod styled;
/// Syntax highlighting integration.
pub mod syntax_highlight;
/// Table formatting utilities.
pub mod table_formatter;
/// Terminal capability detection.
pub mod terminal;
/// Built-in theme definitions and active style accessors.
pub mod theme;
/// Theme configuration file parsing (`.vtcode/theme.toml`).
pub mod theme_config;
/// Theme manager for loading and applying custom themes.
pub mod theme_manager;
/// TUI module re-exports.
pub mod tui;
/// Compatibility layer between core config types and TUI types.
pub mod tui_compat;
/// Global TUI mode flag.
pub mod tui_mode;
/// User confirmation dialogs.
pub mod user_confirmation;

pub use git_config::GitColorConfig;
pub use markdown::*;
pub use search::*;
pub use slash::*;
pub use styled::*;
pub use terminal::*;
pub use theme::*;
pub use theme_config::ThemeConfig;
pub use theme_manager::ThemeManager;
pub use tui::*;
pub use tui_compat::*;
pub use tui_mode::*;
pub use vtcode_ui::tui::ui::FileColorizer;

#[cfg(test)]
mod tests {
    use super::*;
    use anstyle::Effects;

    #[test]
    fn test_render_markdown() {
        let markdown_text = r#"
# Welcome to VT Code

This is a **bold** statement and this is *italic*.

## Features

- Advanced code analysis
- Multi-language support
- Real-time collaboration
"#;

        let rendered = render_markdown(markdown_text);
        let rendered_text = rendered
            .iter()
            .map(|line| line.segments.iter().map(|segment| segment.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered_text.contains("Welcome to VT Code"));
        assert!(!rendered_text.contains("# Welcome to VT Code"));
        assert!(rendered_text.contains("• Advanced code analysis"));
        assert!(
            rendered.iter().flat_map(|line| &line.segments).any(|segment| {
                segment.text.as_str() == "bold" && segment.style.get_effects().contains(Effects::BOLD)
            }),
            "strong markdown should produce a bold segment: {rendered_text:?}"
        );
        assert!(
            rendered.iter().flat_map(|line| &line.segments).any(|segment| {
                segment.text.as_str() == "italic" && segment.style.get_effects().contains(Effects::ITALIC)
            }),
            "emphasis markdown should produce an italic segment: {rendered_text:?}"
        );
    }
}

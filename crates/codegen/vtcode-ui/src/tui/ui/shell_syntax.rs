//! Shell syntax helpers for `• Ran` tool-call lines.
//!
//! Extracted from `src/agent/runloop/unified/tool_pipeline/pty_stream/segments.rs`
//! so both live PTY rendering and the compact-activity row share one
//! tokenizer + palette. Keeps command/args/option/keyword colouring DRY.

use std::sync::Arc;

use anstyle::{AnsiColor, Color as AnsiColourEnum, Effects, Style as AnsiStyle};
use vtcode_commons::ui_protocol::{InlineSegment, InlineTextStyle, convert_style};

use crate::tui::ui::syntax_highlight;

pub struct ShellLineStyles {
    pub output: Arc<InlineTextStyle>,
    pub bullet: Arc<InlineTextStyle>,
    pub glyph: Arc<InlineTextStyle>,
    pub verb: Arc<InlineTextStyle>,
    pub command: Arc<InlineTextStyle>,
    pub args: Arc<InlineTextStyle>,
    pub keyword: Arc<InlineTextStyle>,
    pub variable: Arc<InlineTextStyle>,
    pub string: Arc<InlineTextStyle>,
    pub option: Arc<InlineTextStyle>,
    pub truncation: Arc<InlineTextStyle>,
    /// Structural tokens (`|`, `;`, `&&`, redirections) — muted so command
    /// words and args carry the color hierarchy.
    pub separator: Arc<InlineTextStyle>,
    /// Grouped `N commands` counts — subtle accent, one step below the verb.
    pub count: Arc<InlineTextStyle>,
}

impl ShellLineStyles {
    /// Styles derived from the process-global UI theme (used by PTY live view
    /// when no session is available). Mirrors
    /// `PtyLineStyles::new()` in the binary.
    pub fn new() -> Self {
        let theme_styles = crate::theme::active_styles();
        Self::from_ansi_styles(theme_styles.primary, theme_styles.pty_output)
    }

    /// Styles derived from a session's resolved theme — preferred inside
    /// `AppSession`/`Session` where `InlineTheme` is already available.
    pub fn from_session(_session: &crate::tui::core_tui::app::session::AppSession) -> Self {
        let theme_styles = crate::theme::active_styles();
        // Keep verb synced with the session's primary, body with pty_output
        // so compact rows track theme changes (e.g. catppuccin-latte).
        Self::from_ansi_styles(theme_styles.primary, theme_styles.pty_output)
    }

    fn from_ansi_styles(primary: AnsiStyle, pty_output: AnsiStyle) -> Self {
        let output = Arc::new(convert_style(pty_output));
        let magenta_bold = Arc::new(convert_style(
            AnsiStyle::new()
                .fg_color(Some(AnsiColourEnum::Ansi(AnsiColor::Magenta)))
                .effects(Effects::BOLD),
        ));
        let accent_bold = Arc::new(convert_style(primary | Effects::BOLD));
        let yellow = Arc::new(convert_style(AnsiStyle::new().fg_color(Some(AnsiColourEnum::Ansi(AnsiColor::Yellow)))));

        Self {
            output: Arc::clone(&output),
            bullet: Arc::new(convert_style(AnsiStyle::new().fg_color(Some(AnsiColourEnum::Ansi(AnsiColor::Green))))),
            glyph: Arc::clone(&output),
            verb: accent_bold,
            command: Arc::new(convert_style(
                AnsiStyle::new()
                    .fg_color(Some(AnsiColourEnum::Ansi(AnsiColor::Green)))
                    .effects(Effects::BOLD),
            )),
            args: Arc::new(convert_style(
                AnsiStyle::new()
                    .fg_color(Some(AnsiColourEnum::Ansi(AnsiColor::White)))
                    .effects(Effects::DIMMED),
            )),
            keyword: magenta_bold,
            variable: Arc::clone(&yellow),
            string: yellow,
            option: Arc::new(convert_style(AnsiStyle::new().fg_color(Some(AnsiColourEnum::Ansi(AnsiColor::Red))))),
            truncation: Arc::new(convert_style(pty_output)),
            separator: Arc::new(convert_style(pty_output | Effects::DIMMED)),
            count: Arc::new(convert_style(primary | Effects::DIMMED)),
        }
    }
}

impl Default for ShellLineStyles {
    fn default() -> Self {
        Self::new()
    }
}

fn is_bash_keyword(token: &str) -> bool {
    matches!(
        token,
        "if" | "then"
            | "else"
            | "elif"
            | "fi"
            | "for"
            | "in"
            | "do"
            | "done"
            | "while"
            | "until"
            | "case"
            | "esac"
            | "function"
            | "select"
            | "time"
            | "coproc"
            | "{"
            | "}"
            | "[["
            | "]]"
    )
}

fn is_command_separator(token: &str) -> bool {
    matches!(token, "|" | "||" | "&&" | ";" | ";;" | "&")
}

/// Redirection operators, including fd-prefixed and target-attached forms:
/// `>`, `>>`, `2>`, `2>&1`, `2>/dev/null`, `<`, `<<`.
fn is_redirection_token(token: &str) -> bool {
    let body = token.trim_start_matches(|c: char| c.is_ascii_digit());
    body.starts_with('>') || body.starts_with('<')
}

pub fn tokenize_preserve_whitespace(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut token_start: Option<usize> = None;
    let mut token_is_whitespace = false;

    for (idx, ch) in text.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' && !in_single {
            escaped = true;
        } else if ch == '\'' && !in_double {
            in_single = !in_single;
        } else if ch == '"' && !in_single {
            in_double = !in_double;
        }

        let is_whitespace = !in_single && !in_double && ch.is_whitespace();
        match token_start {
            None => {
                token_start = Some(idx);
                token_is_whitespace = is_whitespace;
            }
            Some(start) if token_is_whitespace != is_whitespace => {
                parts.push(&text[start..idx]);
                token_start = Some(idx);
                token_is_whitespace = is_whitespace;
            }
            _ => {}
        }
    }

    if let Some(start) = token_start {
        parts.push(&text[start..]);
    }

    parts
}

fn style_for_token<'a>(token: &'a str, expect_command: &mut bool, styles: &'a ShellLineStyles) -> Arc<InlineTextStyle> {
    if token.trim().is_empty() {
        return Arc::clone(&styles.output);
    }

    if is_command_separator(token) {
        *expect_command = true;
        return Arc::clone(&styles.separator);
    }

    if is_redirection_token(token) {
        *expect_command = false;
        return Arc::clone(&styles.separator);
    }

    if token.starts_with('"') || token.starts_with('\'') || token.ends_with('"') || token.ends_with('\'') {
        *expect_command = false;
        return Arc::clone(&styles.string);
    }

    if token.starts_with('$') || token.contains("=$") || token.starts_with("${") {
        *expect_command = false;
        return Arc::clone(&styles.variable);
    }

    if token.starts_with('-') && token.len() > 1 {
        *expect_command = false;
        return Arc::clone(&styles.option);
    }

    if is_bash_keyword(token) {
        *expect_command = true;
        return Arc::clone(&styles.keyword);
    }

    if *expect_command {
        *expect_command = false;
        return Arc::clone(&styles.command);
    }

    Arc::clone(&styles.args)
}

fn bash_segments(text: &str, styles: &ShellLineStyles, expect_command: bool) -> Vec<InlineSegment> {
    let mut segments = Vec::new();
    let mut command_expected = expect_command;
    for token in tokenize_preserve_whitespace(text) {
        segments.push(InlineSegment {
            text: token.to_string(),
            style: style_for_token(token, &mut command_expected, styles),
        });
    }
    segments
}

pub fn shell_syntax_segments(text: &str, styles: &ShellLineStyles, expect_command: bool) -> Vec<InlineSegment> {
    let semantic = bash_segments(text, styles, expect_command);
    let Some(highlighted) = syntax_highlight::highlight_line_to_anstyle_segments(
        text,
        Some("bash"),
        syntax_highlight::get_active_syntax_theme(),
        true,
    ) else {
        return semantic;
    };

    if highlighted.is_empty() {
        return semantic;
    }

    let converted = highlighted
        .into_iter()
        .map(|(style, text)| InlineSegment {
            text,
            style: Arc::new(convert_style(style).merge_colour(styles.args.colour)),
        })
        .collect::<Vec<_>>();

    let converted_text = converted.iter().map(|segment| segment.text.as_str()).collect::<String>();
    if converted_text != text {
        return semantic;
    }

    let non_ws_count = semantic.iter().filter(|segment| !segment.text.trim().is_empty()).count();
    if non_ws_count > 1 {
        let mut first_colours: Option<(Option<AnsiColourEnum>, Option<AnsiColourEnum>)> = None;
        let mut has_distinct = false;
        for style in converted
            .iter()
            .filter(|segment| !segment.text.trim().is_empty())
            .map(|segment| segment.style.as_ref())
        {
            let colours = (style.colour, style.bg_colour);
            if let Some(seed) = first_colours {
                if colours != seed {
                    has_distinct = true;
                    break;
                }
            } else {
                first_colours = Some(colours);
            }
        }
        if !has_distinct {
            return semantic;
        }
    }

    converted
}

pub fn line_to_compact_segments(
    metadata: &vtcode_commons::ui_protocol::CompactActivityMetadata,
    styles: &ShellLineStyles,
) -> Vec<InlineSegment> {
    // • Ran <command>  (single) or • Ran N commands (grouped)
    let mut segments = Vec::new();
    segments.push(InlineSegment {
        text: "• ".to_string(),
        style: Arc::clone(&styles.bullet),
    });
    segments.push(InlineSegment {
        text: "Ran".to_string(),
        style: Arc::clone(&styles.verb),
    });
    segments.push(InlineSegment {
        text: " ".to_string(),
        style: Arc::clone(&styles.output),
    });

    if metadata.command_count > 1 {
        // Grouped: subtle accent count for hierarchy — keeps •/Ran prominent.
        segments.push(InlineSegment {
            text: format!("{} commands", metadata.command_count),
            style: Arc::clone(&styles.count),
        });
    } else if let Some(cmd) = metadata.command.as_deref() {
        segments.extend(shell_syntax_segments(cmd, styles, true));
        if metadata.hidden_line_count > 0 {
            let dim_style = Arc::new(InlineTextStyle {
                colour: None,
                bg_colour: None,
                effects: Effects::DIMMED,
            });
            segments.push(InlineSegment {
                text: format!(" · … +{} lines", metadata.hidden_line_count),
                style: dim_style,
            });
        }
    } else {
        segments.push(InlineSegment {
            text: "command".to_string(),
            style: Arc::clone(&styles.args),
        });
    }

    if let Some(suffix) = metadata.suffix.as_deref().filter(|s| !s.is_empty()) {
        let dim_style = Arc::new(InlineTextStyle {
            colour: None,
            bg_colour: None,
            effects: Effects::DIMMED,
        });
        segments.push(InlineSegment {
            text: " · ".to_string(),
            style: Arc::clone(&dim_style),
        });
        segments.push(InlineSegment { text: suffix.to_string(), style: dim_style });
    }

    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_header_preserves_distinct_semantic_token_colours() {
        let styles = ShellLineStyles::new();
        let segments =
            shell_syntax_segments("find src/agent/runloop -maxdepth 3 -type f -name *.rs | sort", &styles, true);
        let option = segments
            .iter()
            .find(|segment| segment.text.contains("maxdepth"))
            .expect("option token");
        let command = segments
            .iter()
            .find(|segment| segment.text.contains("find"))
            .expect("command token");
        assert_ne!(option.style.colour, command.style.colour);
    }

    #[test]
    fn grouped_has_no_single_command_highlight() {
        let styles = ShellLineStyles::new();
        let meta = vtcode_commons::ui_protocol::CompactActivityMetadata {
            group_id: 1,
            command_count: 4,
            command: None,
            hidden_line_count: 10,
            suffix: Some("output retained".into()),
            review_anchor: Some(1),
            review_anchors: vec![1],
        };
        let segs = line_to_compact_segments(&meta, &styles);
        let text: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert!(text.contains("4 commands"));
        assert!(text.contains("output retained"));
    }
}

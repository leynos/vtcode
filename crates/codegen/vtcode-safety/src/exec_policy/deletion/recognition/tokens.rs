//! Token helpers for recognizing `rm` invocations.
//!
//! These helpers scan command text rather than parsed structure, so they
//! deliberately over-refuse on the fail-closed paths.

use std::path::Path;

/// Whether `text` contains an `rm` token in any escaped or quoted spelling.
///
/// This is the fail-closed path for text the parser cannot trust, so it
/// deliberately over-refuses: quote and escape characters are removed before
/// tokenizing, which makes `\rm`, `r\m`, `'rm'`, and `"rm"` all match.
pub(super) fn contains_rm_token(text: &str) -> bool {
    let unquoted: String = text
        .chars()
        .filter(|character| !matches!(character, '\\' | '\'' | '"'))
        .collect();
    unquoted
        .split(|character: char| {
            character.is_whitespace() || matches!(character, ';' | '&' | '|' | '(' | ')' | '<' | '>' | '`')
        })
        .any(|token| base_command_name(token) == "rm")
}

fn strip_wrapping_quotes(token: &str) -> &str {
    token
        .strip_prefix('\'')
        .and_then(|token| token.strip_suffix('\''))
        .or_else(|| token.strip_prefix('"').and_then(|token| token.strip_suffix('"')))
        .unwrap_or(token)
}

pub(super) fn base_command_name(command: &str) -> &str {
    let command = strip_wrapping_quotes(command);
    Path::new(command).file_name().and_then(|name| name.to_str()).unwrap_or(command)
}

pub(super) fn is_shell_interpreter(program: &str) -> bool {
    matches!(program, "bash" | "sh" | "zsh" | "dash" | "ksh")
}

pub(super) fn is_deletion_wrapper(program: &str) -> bool {
    matches!(program, "sudo" | "env" | "command" | "nohup" | "time" | "exec" | "xargs")
}

/// Detect a leading `NAME=value` environment assignment in raw command text.
pub(super) fn starts_with_environment_assignment(command: &str) -> bool {
    command.split_whitespace().next().is_some_and(is_environment_assignment)
}

pub(super) fn is_environment_assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(name, _value)| {
        !name.is_empty()
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
    })
}

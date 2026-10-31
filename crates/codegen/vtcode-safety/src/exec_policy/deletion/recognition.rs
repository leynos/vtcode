//! Synchronous recognition of workspace-scoped `rm` invocations.
//!
//! Recognition is deliberately strict and fails closed. It accepts exactly one
//! simple `rm` command with literal operands and the documented short/long
//! options. Chains, pipes, redirects, wrappers, shell bodies, environment
//! assignments, globs, and shell expansion are reported as
//! [`DeletionRecognition::Unsupported`] so a caller never accidentally treats
//! an unparsed command as a harmless non-deletion.

use std::path::Path;

use crate::command_safety::shell_parser;

/// Short `rm` options accepted by the recognizer.
const RM_SHORT_OPTIONS: &[char] = &['f', 'r', 'R', 'd', 'v', 'i', 'I'];

/// Long `rm` options accepted by the recognizer.
const RM_LONG_OPTIONS: &[&str] = &["--force", "--recursive", "--dir", "--verbose", "--interactive"];

/// The supported shape of a deletion invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionPlan {
    /// Literal operands supplied to `rm`.
    pub operands: Vec<String>,
}

/// Result of classifying a command string as a deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletionRecognition {
    /// The command does not invoke `rm`.
    NotDeletion,
    /// `rm` is present, but the invocation is outside the supported shape.
    Unsupported {
        /// User-readable reason naming what could not be parsed.
        reason: String,
    },
    /// One simple `rm` invocation with literal operands.
    Supported(DeletionPlan),
}

impl DeletionRecognition {
    /// Whether this recognition represents a supported deletion.
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported(_))
    }

    /// Whether this recognition involves `rm` at all.
    pub fn is_deletion(&self) -> bool {
        !matches!(self, Self::NotDeletion)
    }
}

/// Combined recognition of every text a tool call could execute.
///
/// A call may carry the same command in several fields (for example `command`
/// and `raw_command`). The executor is free to prefer any of them, so all
/// representations must agree: a deletion mixed with unrelated text is
/// ambiguous and therefore refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletionCommandSet {
    /// No representation invokes `rm`.
    NotDeletion,
    /// At least one representation is an `rm` invocation outside the
    /// supported shape, or the representations disagree.
    Unsupported {
        /// User-readable reason naming what could not be parsed.
        reason: String,
    },
    /// Every representation is a supported deletion shape.
    Supported(Vec<DeletionPlan>),
}

/// Classify every textual representation of one tool call.
///
/// Returns [`DeletionCommandSet::Unsupported`] when any representation is an
/// unsupported `rm` shape, or when the representations disagree — a mix of
/// deletion and non-deletion text, or two supported deletions with different
/// operands. Only one representation is executed, so the others must not be
/// able to specify something the recognizer never inspected.
pub fn classify_deletion_commands(commands: &[String]) -> DeletionCommandSet {
    let mut plan: Option<DeletionPlan> = None;
    let mut saw_non_deletion = false;

    for command in commands {
        match recognize_deletion_command(command) {
            DeletionRecognition::NotDeletion => saw_non_deletion = true,
            DeletionRecognition::Unsupported { reason } => {
                return DeletionCommandSet::Unsupported { reason };
            }
            DeletionRecognition::Supported(candidate) => match &plan {
                Some(existing) if *existing != candidate => {
                    return DeletionCommandSet::Unsupported {
                        reason: "the call supplies more than one command representation and they do not all describe the same `rm` invocation"
                            .to_string(),
                    };
                }
                Some(_) => {}
                None => plan = Some(candidate),
            },
        }
    }

    match plan {
        None => DeletionCommandSet::NotDeletion,
        Some(_) if saw_non_deletion => DeletionCommandSet::Unsupported {
            reason: "the call supplies more than one command representation and they do not all describe the same `rm` invocation"
                .to_string(),
        },
        Some(plan) => DeletionCommandSet::Supported(vec![plan]),
    }
}

/// Classify a shell command string as a supported, unsupported, or absent
/// `rm` invocation.
///
/// This function is pure and synchronous. It performs no filesystem access;
/// resolved-target checks live in
/// [`assess_deletion_targets`](crate::exec_policy::deletion::assess_deletion_targets).
pub fn recognize_deletion_command(command: &str) -> DeletionRecognition {
    let command = command.trim();
    if command.is_empty() {
        return DeletionRecognition::NotDeletion;
    }

    // Dynamic syntax must fail closed before any parsing: the shell could
    // expand it into an `rm` invocation the parsed text never shows.
    if shell_parser::contains_dynamic_shell_syntax(command) {
        return unsupported_if_rm(
            command,
            "only one static `rm` invocation with literal operands is supported; chaining, pipes, redirects, and shell expansion are not",
        );
    }

    // Control syntax is parsed first so `rm` used only as an argument, as in
    // `rg rm src | head`, is not mistaken for a deletion. A parsed segment
    // that really invokes `rm`, including `rm -f x > out`, stays unsupported.
    if shell_parser::contains_shell_control_syntax(command) {
        return match shell_parser::parse_shell_commands_tree_sitter(command) {
            Ok(commands) => classify_segments(
                &commands,
                "only one static `rm` invocation with literal operands is supported; chaining, pipes, redirects, and shell expansion are not",
            ),
            Err(_) => unsupported_if_rm(
                command,
                "only one static `rm` invocation with literal operands is supported; chaining, pipes, redirects, and shell expansion are not",
            ),
        };
    }

    match shell_parser::parse_shell_commands_tree_sitter(command) {
        Ok(commands) if commands.len() == 1 => {
            let parsed = commands.into_iter().next().unwrap_or_default();
            // Tree-sitter's simple-command extraction drops leading
            // `NAME=value` prefixes, so the raw text decides whether the
            // invocation was environment-prefixed.
            if starts_with_environment_assignment(command)
                && parsed.first().is_some_and(|program| base_command_name(program) == "rm")
            {
                return DeletionRecognition::Unsupported {
                    reason: "environment-prefixed `rm` invocations are not supported; run `rm` directly".to_string(),
                };
            }
            recognize_single_command(&parsed)
        }
        Ok(commands) => classify_segments(
            &commands,
            "only one static `rm` invocation with literal operands is supported; split chained commands and run `rm` directly",
        ),
        Err(_) => unsupported_if_rm(
            command,
            "the command could not be parsed as one static `rm` invocation with literal operands",
        ),
    }
}

/// Refuse parsed segments when any of them invokes `rm`.
fn classify_segments(commands: &[Vec<String>], reason: &str) -> DeletionRecognition {
    if commands.iter().any(|segment| segment_mentions_rm(segment)) {
        DeletionRecognition::Unsupported { reason: reason.to_string() }
    } else {
        DeletionRecognition::NotDeletion
    }
}

fn recognize_single_command(command: &[String]) -> DeletionRecognition {
    let Some(program) = command.first() else {
        return DeletionRecognition::NotDeletion;
    };
    let program_name = base_command_name(program);

    if program_name == "rm" {
        return recognize_rm(command);
    }

    if is_shell_interpreter(program_name) {
        if command.iter().skip(1).any(|argument| contains_rm_token(argument)) {
            return DeletionRecognition::Unsupported {
                reason: "nested `rm` invocations inside a shell body are not supported; run the deletion directly"
                    .to_string(),
            };
        }
        return DeletionRecognition::NotDeletion;
    }

    if is_deletion_wrapper(program_name) || is_environment_assignment(program_name) {
        if command.iter().skip(1).any(|argument| base_command_name(argument) == "rm") {
            return DeletionRecognition::Unsupported {
                reason: "wrapped or environment-prefixed `rm` invocations are not supported; run `rm` directly"
                    .to_string(),
            };
        }
    }

    DeletionRecognition::NotDeletion
}

/// How one `rm` argument participates in the invocation.
enum RmArgument {
    /// `--` ends option parsing.
    Terminator,
    /// A supported option.
    Option,
    /// A literal operand.
    Operand,
    /// An option outside the supported set, with a user-readable reason.
    Rejected(String),
}

fn recognize_rm(command: &[String]) -> DeletionRecognition {
    let mut operands = Vec::new();
    let mut end_of_options = false;

    for argument in command.iter().skip(1) {
        match classify_rm_argument(argument, end_of_options) {
            RmArgument::Terminator => end_of_options = true,
            RmArgument::Option => {}
            RmArgument::Operand => operands.push(argument.clone()),
            RmArgument::Rejected(reason) => return DeletionRecognition::Unsupported { reason },
        }
    }

    if operands.is_empty() {
        return DeletionRecognition::Unsupported {
            reason: "`rm` requires at least one literal operand".to_string(),
        };
    }

    for operand in &operands {
        if let Some(reason) = operand_rejection(operand) {
            return DeletionRecognition::Unsupported { reason: reason.to_string() };
        }
    }

    DeletionRecognition::Supported(DeletionPlan { operands })
}

fn classify_rm_argument(argument: &str, end_of_options: bool) -> RmArgument {
    if end_of_options {
        return RmArgument::Operand;
    }
    if argument == "--" {
        return RmArgument::Terminator;
    }
    if argument.starts_with("--") {
        return classify_long_option(argument);
    }
    match argument.strip_prefix('-') {
        // A bare `-` names a file called `-`, not an option group.
        Some("") | None => RmArgument::Operand,
        Some(flags) => classify_short_options(flags),
    }
}

fn classify_long_option(argument: &str) -> RmArgument {
    if RM_LONG_OPTIONS.contains(&argument) {
        return RmArgument::Option;
    }
    if argument
        .strip_prefix("--interactive=")
        .is_some_and(|value| matches!(value, "always" | "once" | "never"))
    {
        return RmArgument::Option;
    }
    RmArgument::Rejected(format!("unsupported `rm` option `{argument}`"))
}

fn classify_short_options(flags: &str) -> RmArgument {
    match flags.chars().find(|flag| !RM_SHORT_OPTIONS.contains(flag)) {
        Some(unknown) => RmArgument::Rejected(format!("unsupported `rm` option `-{unknown}`")),
        None => RmArgument::Option,
    }
}

fn operand_rejection(operand: &str) -> Option<&'static str> {
    if operand.is_empty() {
        return Some("empty operands are not supported");
    }
    if operand.contains(['*', '?', '[', ']', '{', '}']) {
        return Some("glob operands are not supported; name the exact path");
    }
    if operand.contains(['$', '`']) {
        return Some("shell expansion in operands is not supported");
    }
    if operand == "." {
        return Some("the current directory cannot be deleted");
    }
    if operand.starts_with('~') {
        return Some("home-directory expansion is not supported; use a workspace-relative path");
    }
    if operand.contains('\0') {
        return Some("operands cannot contain NUL bytes");
    }
    None
}

fn unsupported_if_rm(command: &str, reason: &str) -> DeletionRecognition {
    if contains_rm_token(command) {
        DeletionRecognition::Unsupported { reason: reason.to_string() }
    } else {
        DeletionRecognition::NotDeletion
    }
}

/// Whether a parsed segment actually invokes `rm`.
///
/// Arguments are only inspected for wrappers and shell interpreters, whose
/// later arguments name the real command. Elsewhere `rm` is ordinary text:
/// `grep rm file` does not delete anything.
fn segment_mentions_rm(segment: &[String]) -> bool {
    let Some(program) = segment.first() else {
        return false;
    };
    let name = base_command_name(program);
    if name == "rm" {
        return true;
    }
    if is_shell_interpreter(name) {
        // A shell body arrives as one quoted argument, so it must be scanned
        // as text rather than token by token.
        return segment.iter().skip(1).any(|argument| contains_rm_token(argument));
    }
    is_deletion_wrapper(name) && segment.iter().skip(1).any(|argument| base_command_name(argument) == "rm")
}

fn contains_rm_token(text: &str) -> bool {
    text.split(|character: char| {
        character.is_whitespace() || matches!(character, ';' | '&' | '|' | '(' | ')' | '<' | '>' | '`' | '\'' | '"')
    })
    .map(strip_wrapping_quotes)
    .any(|token| base_command_name(token) == "rm")
}

fn strip_wrapping_quotes(token: &str) -> &str {
    token
        .strip_prefix('\'')
        .and_then(|token| token.strip_suffix('\''))
        .or_else(|| token.strip_prefix('"').and_then(|token| token.strip_suffix('"')))
        .unwrap_or(token)
}

fn base_command_name(command: &str) -> &str {
    let command = strip_wrapping_quotes(command);
    Path::new(command).file_name().and_then(|name| name.to_str()).unwrap_or(command)
}

fn is_shell_interpreter(program: &str) -> bool {
    matches!(program, "bash" | "sh" | "zsh" | "dash" | "ksh")
}

fn is_deletion_wrapper(program: &str) -> bool {
    matches!(program, "sudo" | "env" | "command" | "nohup" | "time" | "exec" | "xargs")
}

/// Detect a leading `NAME=value` environment assignment in raw command text.
fn starts_with_environment_assignment(command: &str) -> bool {
    command.split_whitespace().next().is_some_and(is_environment_assignment)
}

fn is_environment_assignment(token: &str) -> bool {
    token.split_once('=').is_some_and(|(name, _value)| {
        !name.is_empty()
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
    })
}

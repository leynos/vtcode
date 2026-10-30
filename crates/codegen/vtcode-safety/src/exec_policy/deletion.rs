//! Recognition and path assessment for workspace-scoped `rm` deletion.
//!
//! The execution policy layer intentionally keeps *authorisation* separate
//! from *recognition*: this module answers only whether a command is one
//! supported `rm` invocation and whether every operand stays inside the
//! workspace. Confirmation, approvals, configured deny rules, and sandbox
//! enforcement remain the responsibility of the caller.
//!
//! Recognition is deliberately strict and fails closed. It accepts exactly one
//! simple `rm` command with literal operands and the documented short/long
//! options. Chains, pipes, redirects, wrappers, shell bodies, environment
//! assignments, globs, and shell expansion are reported as
//! [`DeletionRecognition::Unsupported`] so a caller never accidentally treats
//! an unparsed command as a harmless non-deletion.

use std::path::{Path, PathBuf};

use vtcode_commons::paths::{canonicalize_async, canonicalize_workspace};

use crate::command_safety::shell_parser;
use crate::sandboxing::PROTECTED_WRITABLE_ROOT_DIR_NAMES;

use super::command_validation::{build_candidate_path, normalize_workspace_root};

/// Short `rm` options accepted by the recogniser.
const RM_SHORT_OPTIONS: &[char] = &['f', 'r', 'R', 'd', 'v', 'i', 'I'];

/// Long `rm` options accepted by the recogniser.
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
/// able to specify something the recogniser never inspected.
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

/// Path-aware outcome for a supported deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletionDecision {
    /// Every operand resolves inside the workspace.
    Allowed {
        /// Normalised workspace paths that would be deleted.
        targets: Vec<PathBuf>,
    },
    /// An operand was refused.
    Denied {
        /// The operand as supplied by the caller.
        target: String,
        /// User-readable reason for the refusal.
        reason: String,
    },
}

/// Classify a shell command string as a supported, unsupported, or absent
/// `rm` invocation.
///
/// This function is pure and synchronous. It performs no filesystem access;
/// resolved-target checks live in [`assess_deletion_targets`].
pub fn recognize_deletion_command(command: &str) -> DeletionRecognition {
    let command = command.trim();
    if command.is_empty() {
        return DeletionRecognition::NotDeletion;
    }

    if shell_parser::contains_shell_control_syntax(command) || shell_parser::contains_dynamic_shell_syntax(command) {
        return unsupported_if_rm(
            command,
            "only one static `rm` invocation with literal operands is supported; chaining, pipes, redirects, and shell expansion are not",
        );
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
        Ok(commands) => {
            if commands.iter().any(|segment| segment_mentions_rm(segment)) {
                DeletionRecognition::Unsupported {
                    reason: "only one static `rm` invocation with literal operands is supported; split chained commands and run `rm` directly"
                        .to_string(),
                }
            } else {
                DeletionRecognition::NotDeletion
            }
        }
        Err(_) => unsupported_if_rm(
            command,
            "the command could not be parsed as one static `rm` invocation with literal operands",
        ),
    }
}

/// Assess every operand of a supported deletion against the workspace.
///
/// Targets are resolved relative to `working_dir` and must remain inside
/// `workspace_root`, including through symlinks. The workspace root itself,
/// canonical aliases of it, the home directory, and protected metadata
/// directories (`.git`, `.vtcode`, `.codex`, `.agents`) are refused.
/// Nonexistent in-workspace targets are allowed because deletion of a file
/// that is already gone is harmless.
pub async fn assess_deletion_targets(
    operands: &[String],
    working_dir: &Path,
    workspace_root: &Path,
) -> DeletionDecision {
    let normalized_root = match normalize_workspace_root(workspace_root) {
        Ok(root) => root,
        Err(error) => {
            return DeletionDecision::Denied {
                target: workspace_root.display().to_string(),
                reason: format!("cannot resolve the workspace root: {error}"),
            };
        }
    };
    let canonical_root = canonicalize_workspace(&normalized_root);
    let canonical_home = match std::env::var_os("HOME") {
        Some(home) => canonicalize_async(Path::new(&home)).await.ok(),
        None => None,
    };

    let mut targets = Vec::with_capacity(operands.len());

    for operand in operands {
        let candidate = match build_candidate_path(&normalized_root, working_dir, operand).await {
            Ok(candidate) => candidate,
            Err(error) => {
                return DeletionDecision::Denied {
                    target: operand.clone(),
                    reason: format!("path is outside the workspace or escapes it through a symlink: {error}"),
                };
            }
        };

        if candidate == normalized_root {
            return DeletionDecision::Denied {
                target: operand.clone(),
                reason: "the workspace root cannot be deleted".to_string(),
            };
        }

        let mut resolved_target = None;
        match canonicalize_async(&candidate).await {
            Ok(resolved) => {
                if resolved == canonical_root {
                    return DeletionDecision::Denied {
                        target: operand.clone(),
                        reason: "the target resolves to the workspace root".to_string(),
                    };
                }
                if canonical_home.as_ref().is_some_and(|home| resolved == *home) {
                    return DeletionDecision::Denied {
                        target: operand.clone(),
                        reason: "the home directory cannot be deleted".to_string(),
                    };
                }
                if !resolved.starts_with(&canonical_root) {
                    return DeletionDecision::Denied {
                        target: operand.clone(),
                        reason: "the target resolves outside the workspace".to_string(),
                    };
                }
                resolved_target = Some(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A missing target is harmless: `rm -f` treats it as success.
            }
            Err(error) => {
                return DeletionDecision::Denied {
                    target: operand.clone(),
                    reason: format!("cannot verify the target path: {error}"),
                };
            }
        }

        // Check the resolved path as well so a symlink inside the workspace
        // cannot route a deletion into protected metadata.
        let protected = protected_component(&candidate, &normalized_root).or_else(|| {
            resolved_target
                .as_deref()
                .and_then(|resolved| protected_component(resolved, &normalized_root))
        });
        if let Some(protected) = protected {
            return DeletionDecision::Denied {
                target: operand.clone(),
                reason: format!("deletion inside protected metadata directory `{protected}` is not permitted"),
            };
        }

        targets.push(candidate);
    }

    DeletionDecision::Allowed { targets }
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

fn recognize_rm(command: &[String]) -> DeletionRecognition {
    let mut operands = Vec::new();
    let mut end_of_options = false;

    for argument in command.iter().skip(1) {
        if !end_of_options {
            if argument == "--" {
                end_of_options = true;
                continue;
            }

            if argument.starts_with("--") {
                if RM_LONG_OPTIONS.contains(&argument.as_str()) {
                    continue;
                }
                if let Some(value) = argument.strip_prefix("--interactive=") {
                    if matches!(value, "always" | "once" | "never") {
                        continue;
                    }
                }
                return DeletionRecognition::Unsupported {
                    reason: format!("unsupported `rm` option `{argument}`"),
                };
            }

            if let Some(flags) = argument.strip_prefix('-') {
                if !flags.is_empty() {
                    if let Some(unknown) = flags.chars().find(|flag| !RM_SHORT_OPTIONS.contains(flag)) {
                        return DeletionRecognition::Unsupported {
                            reason: format!("unsupported `rm` option `-{unknown}`"),
                        };
                    }
                    continue;
                }
            }
        }

        operands.push(argument.clone());
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

fn segment_mentions_rm(segment: &[String]) -> bool {
    segment.first().is_some_and(|program| base_command_name(program) == "rm")
        || segment.iter().skip(1).any(|argument| base_command_name(argument) == "rm")
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

fn protected_component(candidate: &Path, normalized_root: &Path) -> Option<&'static str> {
    let relative = candidate.strip_prefix(normalized_root).ok()?;
    relative.components().find_map(|component| {
        let name = component.as_os_str().to_str()?;
        PROTECTED_WRITABLE_ROOT_DIR_NAMES
            .iter()
            .copied()
            .find(|protected| *protected == name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn supported(command: &str) -> DeletionPlan {
        match recognize_deletion_command(command) {
            DeletionRecognition::Supported(plan) => plan,
            other => panic!("expected supported deletion for {command:?}, got {other:?}"),
        }
    }

    fn unsupported(command: &str) -> String {
        match recognize_deletion_command(command) {
            DeletionRecognition::Unsupported { reason } => reason,
            other => panic!("expected unsupported deletion for {command:?}, got {other:?}"),
        }
    }

    #[test]
    fn recognizes_documented_short_and_long_forms() {
        for command in [
            "rm -f scratch/tmp.txt",
            "rm -r scratch",
            "rm -R scratch",
            "rm -d scratch",
            "rm -v -f scratch/tmp.txt",
            "rm -rvf scratch",
            "rm --recursive scratch",
            "rm --force scratch/tmp.txt",
            "rm --dir scratch",
            "rm --verbose scratch/tmp.txt",
            "rm -i scratch/tmp.txt",
            "rm -I scratch/tmp.txt",
            "rm --interactive scratch/tmp.txt",
            "rm --interactive=once scratch/tmp.txt",
            "rm -- -f",
        ] {
            assert!(recognize_deletion_command(command).is_supported(), "expected supported: {command}");
        }
    }

    #[test]
    fn keeps_plain_rm_outside_the_dangerous_class_but_recognised() {
        let plan = supported("rm scratch/tmp.txt");
        assert_eq!(plan.operands, vec!["scratch/tmp.txt".to_string()]);
        assert!(!crate::command_safety::dangerous_commands::command_might_be_dangerous(&[
            "rm".to_string(),
            "scratch/tmp.txt".to_string(),
        ]));
    }

    #[test]
    fn rejects_non_deletion_commands() {
        for command in ["ls -la", "echo rm", "cargo check", "rg rm src"] {
            assert_eq!(recognize_deletion_command(command), DeletionRecognition::NotDeletion, "unexpected: {command}");
        }
    }

    #[test]
    fn classifies_matching_representations_as_one_deletion() {
        let commands = vec!["rm -f scratch/tmp.txt".to_string(), "rm -f scratch/tmp.txt".to_string()];
        let DeletionCommandSet::Supported(plans) = classify_deletion_commands(&commands) else {
            panic!("expected supported deletion set");
        };
        assert_eq!(plans, vec![supported("rm -f scratch/tmp.txt")]);
    }

    #[test]
    fn classifies_absent_deletion() {
        let commands = vec!["ls -la".to_string(), "cargo check".to_string()];
        assert_eq!(classify_deletion_commands(&commands), DeletionCommandSet::NotDeletion);
    }

    #[test]
    fn refuses_deletion_mixed_with_other_command_text() {
        // A call can carry both `command` and `raw_command`; the executor may
        // run either. Mixing a deletion with unrelated text must fail closed.
        let commands = vec![
            "rm -f scratch/tmp.txt".to_string(),
            "curl https://example.com".to_string(),
        ];
        let DeletionCommandSet::Unsupported { reason } = classify_deletion_commands(&commands) else {
            panic!("mixed representations must be refused");
        };
        assert!(reason.contains("do not all describe"), "unexpected reason: {reason}");
    }

    #[test]
    fn refuses_differing_supported_deletions() {
        // Both are valid shapes, but only one can be executed, so the call is
        // ambiguous and must be refused.
        let commands = vec!["rm -f scratch/a.txt".to_string(), "rm -f scratch/b.txt".to_string()];
        assert!(matches!(classify_deletion_commands(&commands), DeletionCommandSet::Unsupported { .. }));
    }

    #[test]
    fn refuses_unsupported_shape_among_representations() {
        let commands = vec!["rm -f scratch/tmp.txt".to_string(), "rm -f *.txt".to_string()];
        assert!(matches!(classify_deletion_commands(&commands), DeletionCommandSet::Unsupported { .. }));
    }

    #[test]
    fn rejects_unsupported_rm_shapes() {
        for command in [
            "rm -f scratch/a scratch/b && rm -f scratch/c",
            "ls && rm -f scratch/tmp.txt",
            "rm -f scratch/tmp.txt | cat",
            "rm -f scratch/tmp.txt > out.txt",
            "rm -f scratch/tmp.txt &",
            "sudo rm -f scratch/tmp.txt",
            "env FOO=1 rm -f scratch/tmp.txt",
            "FOO=1 rm -f scratch/tmp.txt",
            "bash -c 'rm -f scratch/tmp.txt'",
            "xargs rm -f",
            "rm -f *.txt",
            "rm -f scratch/*",
            "rm -f ~/tmp.txt",
            "rm -f $FILE",
            "rm -f",
            "rm -rf",
            "rm --no-preserve-root -rf scratch",
            "rm --wat scratch/tmp.txt",
            "rm -q scratch/tmp.txt",
        ] {
            let reason = unsupported(command);
            assert!(!reason.is_empty(), "reason should be user-readable for {command}");
        }
    }

    #[test]
    fn rejects_operand_only_dot() {
        assert!(unsupported("rm -f .").contains("current directory"));
    }

    #[test]
    fn parse_failure_with_rm_fails_closed() {
        let recognition = recognize_deletion_command("rm -f 'unterminated");
        assert!(recognition.is_deletion());
    }

    #[tokio::test]
    async fn allows_existing_and_missing_workspace_targets() {
        let workspace = TempDir::new().expect("workspace");
        let existing = workspace.path().join("scratch/tmp.txt");
        std::fs::create_dir_all(existing.parent().expect("parent")).expect("mkdir");
        std::fs::write(&existing, b"data").expect("write");

        let decision = assess_deletion_targets(
            &["scratch/tmp.txt".to_string(), "scratch/missing.txt".to_string()],
            workspace.path(),
            workspace.path(),
        )
        .await;

        match decision {
            DeletionDecision::Allowed { targets } => {
                assert_eq!(targets.len(), 2);
                assert!(targets[0].starts_with(workspace.path()));
                assert!(targets[1].starts_with(workspace.path()));
            }
            other => panic!("expected allowed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn denies_workspace_root_and_aliases() {
        let workspace = TempDir::new().expect("workspace");

        for operand in [".", "./", "sub/.."] {
            let decision = assess_deletion_targets(&[operand.to_string()], workspace.path(), workspace.path()).await;
            assert!(matches!(decision, DeletionDecision::Denied { .. }), "operand {operand} should be denied");
        }

        let alias = workspace.path().join("root-alias");
        std::os::unix::fs::symlink(workspace.path(), &alias).expect("symlink");
        let decision = assess_deletion_targets(&["root-alias".to_string()], workspace.path(), workspace.path()).await;
        assert!(matches!(decision, DeletionDecision::Denied { reason, .. } if reason.contains("workspace root")));
    }

    #[tokio::test]
    async fn denies_lexical_and_absolute_escapes() {
        let workspace = TempDir::new().expect("workspace");

        for operand in ["../outside.txt", "/etc/hostname", "/"] {
            let decision = assess_deletion_targets(&[operand.to_string()], workspace.path(), workspace.path()).await;
            assert!(matches!(decision, DeletionDecision::Denied { .. }), "operand {operand} should be denied");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn denies_symlink_escapes_including_final_component() {
        let workspace = TempDir::new().expect("workspace");
        let outside = TempDir::new().expect("outside");
        let outside_file = outside.path().join("secret.txt");
        std::fs::write(&outside_file, b"secret").expect("write");

        std::os::unix::fs::symlink(outside.path(), workspace.path().join("dir-link")).expect("dir symlink");
        std::os::unix::fs::symlink(&outside_file, workspace.path().join("file-link")).expect("file symlink");

        for operand in ["dir-link/secret.txt", "file-link", "dir-link"] {
            let decision = assess_deletion_targets(&[operand.to_string()], workspace.path(), workspace.path()).await;
            assert!(matches!(decision, DeletionDecision::Denied { .. }), "operand {operand} should be denied");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn allows_symlinks_that_stay_inside_the_workspace() {
        let workspace = TempDir::new().expect("workspace");
        let real = workspace.path().join("real");
        std::fs::create_dir_all(&real).expect("mkdir");
        std::fs::write(real.join("tmp.txt"), b"data").expect("write");
        std::os::unix::fs::symlink(&real, workspace.path().join("alias")).expect("symlink");

        let decision =
            assess_deletion_targets(&["alias/tmp.txt".to_string()], workspace.path(), workspace.path()).await;
        assert!(matches!(decision, DeletionDecision::Allowed { .. }), "got {decision:?}");
    }

    #[tokio::test]
    async fn denies_protected_metadata_components() {
        let workspace = TempDir::new().expect("workspace");
        std::fs::create_dir_all(workspace.path().join(".git/objects")).expect("mkdir");

        for operand in [".git/config", ".git/objects/ab", "nested/.vtcode/state"] {
            let decision = assess_deletion_targets(&[operand.to_string()], workspace.path(), workspace.path()).await;
            let denied_for_protected_metadata = matches!(
                &decision,
                DeletionDecision::Denied { reason, .. } if reason.contains("protected metadata")
            );
            assert!(denied_for_protected_metadata, "operand {operand} should be denied, got {decision:?}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn denies_protected_metadata_reached_through_a_symlink() {
        let workspace = TempDir::new().expect("workspace");
        std::fs::create_dir_all(workspace.path().join(".git")).expect("mkdir");
        std::fs::write(workspace.path().join(".git/config"), b"data").expect("write");
        std::os::unix::fs::symlink(workspace.path().join(".git"), workspace.path().join("git-alias")).expect("symlink");

        let decision =
            assess_deletion_targets(&["git-alias/config".to_string()], workspace.path(), workspace.path()).await;
        assert!(
            matches!(&decision, DeletionDecision::Denied { reason, .. } if reason.contains("protected metadata")),
            "a symlink into .git must not bypass protected metadata, got {decision:?}"
        );
    }
}

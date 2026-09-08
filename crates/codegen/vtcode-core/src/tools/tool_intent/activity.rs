//! Shell activity classification for progress accounting and output previews.
//!
//! Mutation safety remains owned by [`super::classify_tool_intent`]. This
//! module adds the narrower distinction between repository inspection and
//! verification without duplicating that safety decision in binary consumers.

use std::path::Path;

use serde_json::Value;

use super::readonly::{
    command_words_are_readonly, static_shell_command_words, static_shell_command_words_with_output_plumbing,
};

/// Progress semantics for a command invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellActivity {
    /// Read-only repository or environment inspection.
    Inspection,
    /// A build, test, lint, or compile command that verifies work.
    Verification,
    /// A command that may mutate state and is not primarily verification.
    Mutation,
}

fn is_verification_invocation(words: &[String]) -> bool {
    let command_words = crate::tools::command_args::command_words_after_environment_prefix(words);
    let first = command_words.first().map(String::as_str).unwrap_or_default();
    let second = command_words.get(1).map(|word| word.to_ascii_lowercase());
    let third = command_words.get(2).map(|word| word.to_ascii_lowercase());
    let program = Path::new(first)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(first)
        .to_ascii_lowercase();

    match program.as_str() {
        "cargo" => {
            matches!(second.as_deref(), Some("check" | "build" | "clippy" | "test"))
                || (second.as_deref() == Some("nextest") && third.as_deref() == Some("run"))
        }
        "go" => matches!(second.as_deref(), Some("test" | "build")),
        "npm" | "pnpm" | "yarn" => {
            matches!(second.as_deref(), Some("test" | "build"))
                || (second.as_deref() == Some("run") && matches!(third.as_deref(), Some("test" | "build")))
        }
        "rustc" | "pytest" | "xcodebuild" | "gradle" | "gradlew" => true,
        _ if first.ends_with("/scripts/check.sh") || first.ends_with("/scripts/check-dev.sh") => true,
        _ => false,
    }
}

fn contains_verification_invocation(command: &str) -> bool {
    static_shell_command_words(command)
        .is_some_and(|commands| commands.iter().any(|words| is_verification_invocation(words)))
}

/// Return whether a shell tool call is an admitted truncation-only verification
/// attempt while the anti-blind-editing gate is pending.
///
/// Piped verifiers (e.g. `cargo check 2>&1 | head -c 4000`) must be allowed to
/// run so the model can see the failure; otherwise the generic "cap output
/// with `| head`" guidance deadlocks on `Mutation blocked until verification`.
/// Only a standalone successful verifier clears the gate; this helper only
/// decides admission, never clearance.
///
/// Fail-closed smuggling guard: every parsed shell segment must be a
/// verification invocation or an allow-listed readonly command. A chained
/// mutation such as `cargo check && rm -rf target` therefore stays blocked
/// instead of riding through on the verifier prefix. Unparsable (dynamic)
/// shell syntax also stays blocked.
pub fn shell_command_is_admitted_verification_attempt(args: &Value) -> bool {
    let Some(command) = crate::tools::command_args::raw_command_text(args) else {
        return false;
    };
    if crate::tools::command_args::contains_dynamic_shell_syntax(&command) {
        return false;
    }
    let segments =
        static_shell_command_words(&command).or_else(|| static_shell_command_words_with_output_plumbing(&command));
    let Some(segments) = segments else {
        return false;
    };
    if segments.is_empty() {
        return false;
    }
    let mut saw_verification = false;
    for words in &segments {
        if is_verification_invocation(words) {
            saw_verification = true;
        } else if !command_words_are_readonly(words) {
            return false;
        }
    }
    saw_verification
}

fn has_logical_sequencing(words: &[String]) -> bool {
    words.iter().any(|word| matches!(word.as_str(), "&&" | "||" | ";"))
}

fn is_known_inspection(words: &[String]) -> bool {
    if words
        .iter()
        .any(|word| matches!(word.as_str(), ">" | ">>" | "|" | "&&" | ";" | "||"))
    {
        return false;
    }
    command_words_are_readonly(words)
}

fn classify_provable_shell_sequence(command: &str) -> Option<ShellActivity> {
    let (segments, has_output_plumbing) = if let Some(segments) = static_shell_command_words(command) {
        (segments, false)
    } else {
        (static_shell_command_words_with_output_plumbing(command)?, true)
    };
    if has_output_plumbing && segments.len() != 1 {
        return None;
    }
    let has_multiple_segments = segments.len() > 1;
    let mut saw_verification = false;

    for words in segments {
        if is_verification_invocation(&words) {
            saw_verification = true;
        } else if !command_words_are_readonly(&words) {
            return None;
        }
    }

    if has_output_plumbing && !saw_verification {
        return None;
    }

    // Shell execution does not guarantee that a pipeline or logical chain's
    // final status reflects every verification stage. Do not let a successful
    // downstream command clear the anti-blind checkpoint after an earlier
    // verifier failed.
    if saw_verification && has_multiple_segments {
        return Some(ShellActivity::Mutation);
    }

    Some(if saw_verification {
        ShellActivity::Verification
    } else {
        ShellActivity::Inspection
    })
}

fn has_shell_sequence(command: &str) -> bool {
    static_shell_command_words(command).is_none_or(|segments| segments.len() > 1)
}

/// Classify a shell call without weakening the authoritative mutation guard.
///
/// Standalone output plumbing such as `2>&1` or `> build.log` does not turn a
/// primary verification command into a mutation for progress accounting.
/// Multi-stage pipelines and chains remain mutations because their final
/// status does not reliably represent every verification stage.
#[must_use]
pub fn classify_shell_activity(tool_name: &str, args: &Value) -> ShellActivity {
    let command = crate::tools::command_args::raw_command_text(args);
    let words = crate::tools::command_args::command_words(args).ok().flatten();
    let has_unclassified_shell_sequence = command.as_deref().is_some_and(has_shell_sequence);

    if let Some(activity) = command.as_deref().and_then(classify_provable_shell_sequence) {
        return activity;
    }

    let intent = super::classify_tool_intent(tool_name, args);

    if !has_unclassified_shell_sequence && words.as_deref().is_some_and(is_known_inspection) {
        return ShellActivity::Inspection;
    }

    let starts_with_verification = words.as_deref().is_some_and(is_verification_invocation);
    let contains_verification =
        starts_with_verification || command.as_deref().is_some_and(contains_verification_invocation);
    if !intent.mutating {
        return if contains_verification {
            ShellActivity::Verification
        } else {
            ShellActivity::Inspection
        };
    }

    if starts_with_verification
        && !has_unclassified_shell_sequence
        && !words.as_deref().is_some_and(has_logical_sequencing)
    {
        ShellActivity::Verification
    } else {
        ShellActivity::Mutation
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::constants::tools;
    #[cfg(unix)]
    use crate::tools::tool_intent::is_readonly_command_session_command;

    fn exec_command(command: &str) -> Value {
        json!({"cmd": command})
    }

    #[test]
    fn admitted_verification_attempt_allows_truncation_but_blocks_smuggled_mutations() {
        for command in [
            "cargo check --locked 2>&1 | head -c 4000",
            "cargo check --locked",
            "cargo nextest run 2>&1 | head -c 4000",
        ] {
            assert!(
                shell_command_is_admitted_verification_attempt(&exec_command(command)),
                "expected admission: {command}"
            );
        }
        for command in [
            "cargo check && rm -rf target",
            "cargo check; rm foo.txt",
            "cargo check || rm foo.txt",
            "cargo check && cargo test && rm foo.txt",
            "sed -i '' 's/old/new/' README.md",
            "echo $(date)",
            "cargo check > build.log && rm foo.txt",
        ] {
            assert!(
                !shell_command_is_admitted_verification_attempt(&exec_command(command)),
                "expected block: {command}"
            );
        }
        assert!(!shell_command_is_admitted_verification_attempt(&json!({})));
    }

    #[test]
    fn logged_compound_inspection_commands_are_not_mutations() {
        for command in [
            "cat README.md && printf '\\n--- git status ---\\n' && git status --short",
            "wc -l README.md; rg -n '^#' README.md",
            "git diff --stat; find docs -maxdepth 2 -type f | sort | head -40",
        ] {
            assert_eq!(
                classify_shell_activity(tools::EXEC_COMMAND, &exec_command(command)),
                ShellActivity::Inspection,
                "{command}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn captured_read_commands_with_output_suppression_are_inspection() {
        for command in [
            r#"sed -n '1,180p' README.md; sed -n '280,350p' README.md; sed -n '389,411p' README.md; printf '\n--- repo metadata ---\n'; git log -1 --format='%h %s'; sed -n '1,100p' Cargo.toml; rg -n '^version\s*=|rust-version|workspace\.package' Cargo.toml crates -g Cargo.toml | head -40"#,
            r#"sed -n '1,120p' crates/codegen/vtcode-core/src/tools/tool_intent/activity.rs; printf '\n--- readonly policy ---\n'; rg -n 'READONLY_UNIFIED_EXEC_COMMANDS|command_words_are_readonly' crates/codegen/vtcode-core/src/tools/tool_intent/readonly.rs; printf '\n--- recent commits ---\n'; git log -5 --oneline; printf '\n--- command arguments ---\n'; sed -n '1,180p' crates/codegen/vtcode-core/src/tools/command_args.rs"#,
            r###"git diff --stat; find docs -maxdepth 2 -type f | sort | head -40; rg -n "vtcode init|vtcode models|full-auto|run-debug|cargo install" docs/user-guide docs/installation docs/development 2>/dev/null | head -50"###,
        ] {
            assert_eq!(
                classify_shell_activity(tools::EXEC_COMMAND, &exec_command(command)),
                ShellActivity::Inspection,
                "{command}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn printf_output_safety_guards_remain_mutations() {
        for command in [
            "printf 'captured output\\n' > output.txt",
            "printf '%s\\n' \"$(git status --short)\"",
            "printf '%s\\n' `git status --short`",
            "printf '\\n--- inspection ---\\n' && rm output.txt",
        ] {
            let args = exec_command(command);
            assert_eq!(classify_shell_activity(tools::EXEC_COMMAND, &args), ShellActivity::Mutation, "{command}");
            assert!(!is_readonly_command_session_command(&args), "unexpected readonly command: {command}");
        }
    }

    #[test]
    fn git_diff_check_chain_remains_inspection() {
        assert_eq!(
            classify_shell_activity(
                tools::EXEC_COMMAND,
                &exec_command("git diff --check && git status --short && git diff --stat"),
            ),
            ShellActivity::Inspection
        );
    }

    #[test]
    fn verification_detection_skips_environment_prefixes() {
        for command in [
            "env RUSTFLAGS=-Dwarnings cargo check",
            "RUSTFLAGS=-Dwarnings env cargo check",
            "env -u PATH cargo check",
            "env -C /tmp cargo check",
        ] {
            assert_eq!(
                classify_shell_activity(tools::EXEC_COMMAND, &exec_command(command)),
                ShellActivity::Verification,
                "{command}"
            );
        }
    }

    #[test]
    fn ambiguous_or_mutating_compounds_remain_mutations() {
        for command in [
            "git diff --stat; python3 -c 'open(\"out\", \"w\").write(\"x\")'",
            "cat README.md; sed -i '' 's/a/b/' README.md",
            "sed --in-place= README.md",
            "git diff --output=out",
            "git diff '--output=out'",
            "git diff -o out",
            concat!("git diff -o", "out"),
            "git log --output=out",
            "git show --textconv",
            "git -C /external/repo=alt status",
            "find . -fprint output.txt",
            "find . -fprintf output.txt '%p'",
            "rg --hostname-bin sh pattern",
            "rg --search-zip pattern",
            "rg -z pattern",
            "sort -o generated.txt README.md",
            "sort --compress-program=sh README.md",
            "date -s now",
            "awk -i inplace '{print}' README.md",
            "sed -n 's/a/b/e' README.md",
            "fd --exec sh -c 'touch output'",
            "tree -o output.txt",
            "ast-grep -r 'README.md'",
            "sed -n -fmalicious.sed -e '1p' src/main.rs",
            "sed -I '' 's/a/b/' src/main.rs",
            "sed -n '1p\nw leaked.txt' src/main.rs",
            "cargo check & rm output",
            "cargo check > build.log | rm output",
            "cargo check | head -40 > build.log",
            "cargo check | echo x > output.log",
            "cargo check | head -40",
            "cargo check > build.log &",
            "env -S 'cargo check'",
            "echo x > output.log && cargo check",
            "cargo check < build-input.log",
            "cat README.md > copied.txt",
            "git diff --check; rm output",
            "cat README.md\nrm output",
        ] {
            assert_eq!(
                classify_shell_activity(tools::EXEC_COMMAND, &exec_command(command)),
                ShellActivity::Mutation,
                "{command}"
            );
        }
    }

    #[test]
    fn quoted_output_text_does_not_change_inspection_classification() {
        for command in ["echo 'git diff --output=out'", "printf 'sort -o out input'"] {
            assert_eq!(
                classify_shell_activity(tools::EXEC_COMMAND, &exec_command(command)),
                ShellActivity::Inspection,
                "{command}"
            );
        }
    }
}

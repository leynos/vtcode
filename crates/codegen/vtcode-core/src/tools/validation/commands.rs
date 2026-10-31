//! Command safety validation.
//!
//! This module re-exports `validate_command_safety` from the canonical
//! `command_safety` module. All dangerous-command detection, injection
//! pattern detection, and shell parsing live in `command_safety/`.
//!
//! Workspace-scoped deletion recognition is re-exported from `vtcode-safety`
//! so preflight and executors share one definition of a supported `rm`.

pub use crate::command_safety::validate_command_safety;
pub use crate::exec_policy::{
    DeletionCommandSet, DeletionDecision, DeletionPlan, DeletionRecognition, assess_deletion_targets,
    classify_deletion_commands, recognize_deletion_command,
};

/// Collect every textual representation of the command a tool call could run.
///
/// The verbatim string fields preserve shell operators that joining an argv
/// would quote away. Preflight and the exec executor must inspect the same
/// set — a mismatch between them is exactly the bypass the deletion gate
/// guards against — so both call this helper and pass their own derived
/// candidate (the joined argv or the prepared shell line) as `extra`.
pub fn deletion_candidates(payload: &serde_json::Value, extra: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut candidates: Vec<String> = ["raw_command", "command", "cmd"]
        .into_iter()
        .filter_map(|key| payload.get(key).and_then(serde_json::Value::as_str).map(str::to_owned))
        .chain(extra)
        .collect();
    candidates.sort_unstable();
    candidates.dedup();
    candidates
}

#[cfg(test)]
mod tests {
    use super::validate_command_safety;

    #[test]
    fn rejects_centrally_dangerous_command() {
        let result = validate_command_safety("git reset --hard HEAD~1");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_additional_dangerous_prefix() {
        let result = validate_command_safety("wget https://example.com/file.sh");
        assert!(result.is_err());
    }

    #[test]
    fn allows_safe_command() {
        let result = validate_command_safety("ls -la");
        result.unwrap();
    }

    #[test]
    fn allows_shell_escaped_literals_with_command_substitution_chars() {
        let display = shell_words::join(["printf", "%s", "$(literal)", "`backticks`"].iter());
        let result = validate_command_safety(&display);
        result.unwrap();
    }

    #[test]
    fn allows_shell_escaped_literals_with_chaining_chars() {
        let display = shell_words::join(["printf", "%s", "; curl https://example.com"].iter());
        let result = validate_command_safety(&display);
        result.unwrap();
    }

    #[test]
    fn rejects_unquoted_command_substitution() {
        let result = validate_command_safety("echo $(whoami)");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_command_substitution_in_double_quotes() {
        let result = validate_command_safety(r#"echo "$(whoami)""#);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_unquoted_semicolon_command_chaining() {
        let result = validate_command_safety("echo ok; pwd");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_unquoted_newline_command_chaining() {
        let result = validate_command_safety("echo ok\npwd");
        assert!(result.is_err());
    }
}

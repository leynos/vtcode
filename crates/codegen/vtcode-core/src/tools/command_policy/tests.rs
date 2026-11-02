//! Configured command-policy evaluation and denial diagnostics.

use super::*;
use crate::config::CommandsConfig;

#[test]
fn glob_allows_cargo_commands() {
    let mut config = CommandsConfig::default();
    config.allow_list.clear();
    config.allow_regex.clear();
    config.allow_glob = vec!["cargo *".to_string()];
    let evaluator = CommandPolicyEvaluator::from_config(&config);
    assert!(evaluator.allows_text("cargo fmt"));
    assert!(evaluator.allows(&["cargo".into(), "check".into()]));
}

#[test]
fn glob_supports_question_mark() {
    let mut config = CommandsConfig::default();
    config.allow_list.clear();
    config.allow_regex.clear();
    config.allow_glob = vec!["go test ./pkg/?".to_string()];
    let evaluator = CommandPolicyEvaluator::from_config(&config);
    assert!(evaluator.allows_text("go test ./pkg/a"));
    assert!(!evaluator.allows_text("go test ./pkg/ab"));
}

#[test]
fn glob_allows_node_ecosystem_commands() {
    let mut config = CommandsConfig::default();
    config.allow_list.clear();
    config.allow_regex.clear();
    config.allow_glob = vec!["npm *".to_string(), "bun *".to_string()];
    let evaluator = CommandPolicyEvaluator::from_config(&config);
    assert!(evaluator.allows_text("npm install"));
    assert!(evaluator.allows_text("npm run build"));
    assert!(evaluator.allows_text("bun install"));
    assert!(evaluator.allows_text("bun run check"));
}

/// The default policy allows `rm` only as a whole command token, so
/// `rmdir`-style neighbours stay fail-closed, and the shipped deny rules
/// still win over the allow glob.
#[test]
fn default_policy_allows_workspace_rm_without_widening_similar_commands() {
    let config = CommandsConfig::default();
    let evaluator = CommandPolicyEvaluator::from_config(&config);

    assert!(evaluator.allows_text("rm -f scratch/tmp.txt"));
    assert!(evaluator.allows_text("rm scratch/tmp.txt"));
    assert!(!evaluator.allows_text("rmdir scratch"));
    assert!(!evaluator.allows_text("rmx scratch"));
    assert!(!evaluator.allows_text("rm -rf /"));
    assert!(!evaluator.allows_text("rm --force scratch/tmp.txt"));
    // Recursive and forced removals stay denied in any flag order.
    for command in [
        "rm -r scratch",
        "rm -fr scratch",
        "rm -Rf scratch",
        "rm -f -r scratch",
        "rm --recursive scratch",
    ] {
        assert!(!evaluator.allows_text(command), "expected denial: {command}");
    }
}

/// The default deny regex matches `rm` as a command word, not as the
/// suffix of another command name such as `terraform`.
#[test]
fn default_deny_regex_matches_rm_as_a_word_only() {
    let mut config = CommandsConfig::default();
    config.allow_glob.push("terraform *".to_string());
    let evaluator = CommandPolicyEvaluator::from_config(&config);

    assert!(evaluator.allows_text("terraform -r apply"));
    assert!(!evaluator.allows_text("rm -r scratch"));
    assert!(!evaluator.allows_text("ls && rm -r scratch"));
}

/// Regression for issue #175: the shipped policy allows the two local
/// GitHub CLI version queries and records the exact matching rule.
#[test]
fn default_policy_allows_gh_version_queries_exactly() {
    let config = CommandsConfig::default();
    let evaluator = CommandPolicyEvaluator::from_config(&config);

    for command in [
        vec!["gh".to_string(), "--version".to_string()],
        vec!["gh".to_string(), "version".to_string()],
    ] {
        let expected_text = command.join(" ");
        assert!(evaluator.allows(&command), "expected allowance: {expected_text}");
        assert!(evaluator.allows_text(&expected_text));
        assert_eq!(
            evaluator.decision(&command),
            CommandPolicyDecision::AllowedByRule {
                rule: CommandPolicyRule::AllowGlob,
                pattern: expected_text,
            }
        );
    }
}

/// The version-query exception is an exact match. Composition, wrappers,
/// absolute paths, and every other `gh` operation stay denied by default.
#[test]
fn default_policy_denies_other_gh_invocations() {
    let evaluator = CommandPolicyEvaluator::from_config(&CommandsConfig::default());

    for command in [
        "gh",
        "gh api repos/owner/repo",
        "gh auth status",
        "gh alias list",
        "gh extension list",
        "gh --version && echo ok",
        "gh --version; echo ok",
        "gh --version | cat",
        "gh --version $(echo ok)",
        "bash -lc 'gh --version'",
        "/usr/bin/gh --version",
    ] {
        assert!(!evaluator.allows_text(command), "expected denial: {command}");
    }
}

#[test]
fn decisions_distinguish_deny_rules_from_allowlist_misses() {
    let evaluator = CommandPolicyEvaluator::from_config(&CommandsConfig::default());

    assert_eq!(
        evaluator.decision_text("rm -rf /"),
        CommandPolicyDecision::DeniedByRule {
            rule: CommandPolicyRule::DenyList,
            pattern: "rm -rf /".to_string(),
        }
    );
    assert_eq!(
        evaluator.decision_text("curl https://example.com"),
        CommandPolicyDecision::DeniedWithoutAllowRuleMatch
    );
    assert!(evaluator.decision_text("git status").is_allowed());
}

#[test]
fn empty_allow_rules_allow_unmatched_commands() {
    let mut config = CommandsConfig::default();
    config.allow_list.clear();
    config.allow_regex.clear();
    config.allow_glob.clear();
    let evaluator = CommandPolicyEvaluator::from_config(&config);

    assert_eq!(evaluator.decision_text("gh api repos/owner/repo"), CommandPolicyDecision::AllowedWithoutAllowRules);
}

#[test]
fn denial_message_names_program_and_category_without_arguments() {
    let evaluator = CommandPolicyEvaluator::from_config(&CommandsConfig::default());

    let decision = evaluator.decision(&["gh".to_string(), "auth".to_string(), "token".to_string()]);
    let message = decision.denial_message("gh").expect("expected a denial message");
    assert!(message.starts_with("command 'gh' is not permitted by the execution policy"));
    assert!(message.contains("no [commands] allow_list, allow_glob, or allow_regex rule matches"));
    assert!(message.contains("--dangerously-skip-permissions does not override the command policy"));
    assert!(!message.contains("token"));

    let denied = evaluator.decision_text("rm -rf /");
    let message = denied.denial_message("/usr/bin/rm").expect("expected a denial message");
    assert!(message.starts_with("command 'rm' is not permitted by the execution policy"));
    assert!(message.contains("blocked by the [commands] deny_list rule 'rm -rf /'"));
}

#[test]
fn allow_list_allows_exact_git_and_cargo_commands() {
    let mut config = CommandsConfig::default();
    // Clear default allow_list to reduce noise
    config.allow_list.clear();
    config.allow_list.push("git".to_string());
    config.allow_list.push("cargo".to_string());
    let evaluator = CommandPolicyEvaluator::from_config(&config);
    assert!(evaluator.allows_text("git"));
    assert!(evaluator.allows_text("cargo"));
    assert!(evaluator.allows(&["git".into()]));
    assert!(evaluator.allows(&["cargo".into()]));
}

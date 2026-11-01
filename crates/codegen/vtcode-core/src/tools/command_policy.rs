use crate::audit::PermissionDecision;
use crate::config::CommandsConfig;
use crate::tools::command_cache::PermissionCache;
use crate::tools::command_resolver::CommandResolver;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::warn;

/// A compiled configured rule together with the pattern text it came from.
#[derive(Debug, Clone)]
struct CompiledPattern {
    pattern: String,
    regex: Regex,
}

/// Rule collection that produced a [`CommandPolicyDecision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPolicyRule {
    AllowList,
    DenyList,
    AllowGlob,
    DenyGlob,
    AllowRegex,
    DenyRegex,
}

impl CommandPolicyRule {
    /// Configuration key that owns this rule collection.
    pub fn config_key(self) -> &'static str {
        match self {
            Self::AllowList => "allow_list",
            Self::DenyList => "deny_list",
            Self::AllowGlob => "allow_glob",
            Self::DenyGlob => "deny_glob",
            Self::AllowRegex => "allow_regex",
            Self::DenyRegex => "deny_regex",
        }
    }
}

/// Structured outcome of evaluating one command against the configured policy.
///
/// The decision records which rule collection decided so diagnostics can name
/// it without repeating the command text. `denial_message` is secret-safe: it
/// includes configured rule patterns and the program basename only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandPolicyDecision {
    /// A configured allow rule matched.
    AllowedByRule { rule: CommandPolicyRule, pattern: String },
    /// No allow rules are configured, so unmatched commands are allowed.
    AllowedWithoutAllowRules,
    /// A configured deny rule matched; deny rules win over every allow rule.
    DeniedByRule { rule: CommandPolicyRule, pattern: String },
    /// Allow rules are configured and none of them matched.
    DeniedWithoutAllowRuleMatch,
}

impl CommandPolicyDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::AllowedByRule { .. } | Self::AllowedWithoutAllowRules)
    }

    /// Deny rule and configured pattern, when a deny rule decided.
    pub fn denied_by_rule(&self) -> Option<(CommandPolicyRule, &str)> {
        match self {
            Self::DeniedByRule { rule, pattern } => Some((*rule, pattern.as_str())),
            _ => None,
        }
    }

    /// Build the operator-facing denial message.
    ///
    /// `program` is reduced to its basename and no command arguments are
    /// included, so diagnostics cannot leak values passed on the command line.
    pub fn denial_message(&self, program: &str) -> Option<String> {
        let detail = match self {
            Self::AllowedByRule { .. } | Self::AllowedWithoutAllowRules => return None,
            Self::DeniedByRule { rule, pattern } => {
                format!("blocked by the [commands] {} rule '{pattern}'", rule.config_key())
            }
            Self::DeniedWithoutAllowRuleMatch => {
                "no [commands] allow_list, allow_glob, or allow_regex rule matches this command".to_string()
            }
        };
        let program = program_basename(program);
        Some(format!(
            "command '{program}' is not permitted by the execution policy: {detail}; \
             --dangerously-skip-permissions does not override the command policy"
        ))
    }
}

/// Reduce a program path to the basename used in diagnostics.
fn program_basename(program: &str) -> &str {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(program)
}

#[derive(Clone)]
pub struct CommandPolicyEvaluator {
    allow_prefixes: Vec<String>,
    deny_prefixes: Vec<String>,
    allow_regexes: Vec<CompiledPattern>,
    deny_regexes: Vec<CompiledPattern>,
    allow_glob_regexes: Vec<CompiledPattern>,
    deny_glob_regexes: Vec<CompiledPattern>,
    allow_regexes_empty: bool,
    allow_globs_empty: bool,
    // NEW: Command resolution and caching for improved security visibility
    resolver: Arc<Mutex<CommandResolver>>,
    cache: Arc<Mutex<PermissionCache>>,
}

impl CommandPolicyEvaluator {
    pub fn from_config(config: &CommandsConfig) -> Self {
        let allow_prefixes = crate::utils::merge_env_patterns(&config.allow_list, "VTCODE_COMMANDS_ALLOW_LIST");
        let deny_prefixes = crate::utils::merge_env_patterns(&config.deny_list, "VTCODE_COMMANDS_DENY_LIST");

        let allow_regex_patterns = crate::utils::merge_env_patterns(&config.allow_regex, "VTCODE_COMMANDS_ALLOW_REGEX");
        let deny_regex_patterns = crate::utils::merge_env_patterns(&config.deny_regex, "VTCODE_COMMANDS_DENY_REGEX");

        let allow_glob_patterns = crate::utils::merge_env_patterns(&config.allow_glob, "VTCODE_COMMANDS_ALLOW_GLOB");
        let deny_glob_patterns = crate::utils::merge_env_patterns(&config.deny_glob, "VTCODE_COMMANDS_DENY_GLOB");

        let allow_regexes = compile_regexes(&allow_regex_patterns);
        let deny_regexes = compile_regexes(&deny_regex_patterns);
        let allow_glob_regexes = compile_globs(&allow_glob_patterns);
        let deny_glob_regexes = compile_globs(&deny_glob_patterns);

        Self {
            allow_prefixes,
            deny_prefixes,
            allow_regexes,
            deny_regexes,
            allow_glob_regexes,
            deny_glob_regexes,
            allow_regexes_empty: allow_regex_patterns.is_empty(),
            allow_globs_empty: allow_glob_patterns.is_empty(),
            resolver: Arc::new(Mutex::new(CommandResolver::new())),
            cache: Arc::new(Mutex::new(PermissionCache::new())),
        }
    }

    fn cached_decision(&self, command_text: &str) -> Option<bool> {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| {
                warn!("command_policy: permission cache mutex poisoned; recovering");
                poisoned.into_inner()
            })
            .get(command_text)
    }

    fn resolve_path(&self, command_text: &str) -> Option<PathBuf> {
        self.resolver
            .lock()
            .unwrap_or_else(|poisoned| {
                warn!("command_policy: command resolver mutex poisoned; recovering");
                poisoned.into_inner()
            })
            .resolve(command_text)
            .resolved_path
            .clone()
    }

    fn cache_decision(&self, command_text: &str, allowed: bool, reason: &str) {
        let mut cache = self.cache.lock().unwrap_or_else(|poisoned| {
            warn!("command_policy: permission cache mutex poisoned; recovering");
            poisoned.into_inner()
        });
        cache.put(command_text, allowed, reason);
    }

    pub fn allows(&self, command: &[String]) -> bool {
        self.decision(command).is_allowed()
    }

    pub fn allows_text(&self, command_text: &str) -> bool {
        self.decision_text(command_text).is_allowed()
    }

    /// Structured decision for an argv request.
    pub fn decision(&self, command: &[String]) -> CommandPolicyDecision {
        if command.is_empty() {
            return CommandPolicyDecision::DeniedWithoutAllowRuleMatch;
        }
        self.decision_text(&command.join(" "))
    }

    /// Structured decision for a raw command line.
    pub fn decision_text(&self, command_text: &str) -> CommandPolicyDecision {
        let cmd = command_text.trim();
        if cmd.is_empty() {
            return CommandPolicyDecision::DeniedWithoutAllowRuleMatch;
        }

        // Deny takes precedence
        if let Some(pattern) = first_matching_prefix(cmd, &self.deny_prefixes) {
            return CommandPolicyDecision::DeniedByRule {
                rule: CommandPolicyRule::DenyList,
                pattern: pattern.to_string(),
            };
        }
        if let Some(pattern) = first_matching(cmd, &self.deny_regexes) {
            return CommandPolicyDecision::DeniedByRule {
                rule: CommandPolicyRule::DenyRegex,
                pattern: pattern.to_string(),
            };
        }
        if let Some(pattern) = first_matching(cmd, &self.deny_glob_regexes) {
            return CommandPolicyDecision::DeniedByRule {
                rule: CommandPolicyRule::DenyGlob,
                pattern: pattern.to_string(),
            };
        }

        // If no allow rules defined, allow by default
        if self.allow_prefixes.is_empty() && self.allow_regexes_empty && self.allow_globs_empty {
            return CommandPolicyDecision::AllowedWithoutAllowRules;
        }

        // Check allow rules
        if let Some(pattern) = first_matching_prefix(cmd, &self.allow_prefixes) {
            return CommandPolicyDecision::AllowedByRule {
                rule: CommandPolicyRule::AllowList,
                pattern: pattern.to_string(),
            };
        }
        if let Some(pattern) = first_matching(cmd, &self.allow_regexes) {
            return CommandPolicyDecision::AllowedByRule {
                rule: CommandPolicyRule::AllowRegex,
                pattern: pattern.to_string(),
            };
        }
        if let Some(pattern) = first_matching(cmd, &self.allow_glob_regexes) {
            return CommandPolicyDecision::AllowedByRule {
                rule: CommandPolicyRule::AllowGlob,
                pattern: pattern.to_string(),
            };
        }

        CommandPolicyDecision::DeniedWithoutAllowRuleMatch
    }

    /// Enhanced async evaluation with command resolution and caching
    /// Returns (allowed, resolved_path, reason, decision)
    pub fn evaluate_with_resolution(&self, command_text: &str) -> (bool, Option<PathBuf>, String, PermissionDecision) {
        let cmd = command_text.trim();

        // Check cache first
        if let Some(allowed) = self.cached_decision(cmd) {
            let reason = if allowed {
                "Cached allow decision"
            } else {
                "Cached deny decision"
            };
            return (allowed, None, reason.to_string(), PermissionDecision::Cached);
        }

        // Resolve command to actual path
        let resolved_path = self.resolve_path(cmd);

        // Evaluate policy
        let decision = self.decision_text(cmd);
        let allowed = decision.is_allowed();

        // Determine reason from the structured decision. Reasons name the rule
        // collection and its configured pattern, never the command arguments.
        let reason = match &decision {
            CommandPolicyDecision::AllowedByRule { rule, pattern }
            | CommandPolicyDecision::DeniedByRule { rule, pattern } => {
                format!("{} match: {pattern}", rule.config_key())
            }
            CommandPolicyDecision::AllowedWithoutAllowRules => {
                "no allow rules configured; allowed by default".to_string()
            }
            CommandPolicyDecision::DeniedWithoutAllowRuleMatch => "no allow rule matches".to_string(),
        };

        // Cache the result
        self.cache_decision(cmd, allowed, &reason);

        let decision = if allowed {
            PermissionDecision::Allowed
        } else {
            PermissionDecision::Denied
        };

        (allowed, resolved_path, reason, decision)
    }
}

fn first_matching_prefix<'a>(value: &str, prefixes: &'a [String]) -> Option<&'a str> {
    prefixes
        .iter()
        .filter(|pattern| !pattern.is_empty())
        .find(|pattern| value.starts_with(pattern.as_str()))
        .map(String::as_str)
}

fn first_matching<'a>(value: &str, patterns: &'a [CompiledPattern]) -> Option<&'a str> {
    patterns
        .iter()
        .find(|compiled| compiled.regex.is_match(value))
        .map(|compiled| compiled.pattern.as_str())
}

fn compile_regexes(patterns: &[String]) -> Vec<CompiledPattern> {
    patterns
        .iter()
        .filter_map(|pattern| {
            Regex::new(pattern)
                .map(|regex| CompiledPattern { pattern: pattern.clone(), regex })
                .map_err(|error| {
                    warn!(%error, %pattern, "Ignoring invalid command regex pattern");
                    error
                })
                .ok()
        })
        .collect()
}

fn compile_globs(patterns: &[String]) -> Vec<CompiledPattern> {
    patterns
        .iter()
        .filter_map(|pattern| {
            let escaped = regex::escape(pattern);
            let glob_regex = format!("^{}$", escaped.replace(r"\*", ".*").replace(r"\?", "."));
            Regex::new(&glob_regex)
                .map(|regex| CompiledPattern { pattern: pattern.clone(), regex })
                .map_err(|error| {
                    warn!(%error, pattern = %pattern, "Ignoring invalid command glob pattern");
                    error
                })
                .ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
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
}

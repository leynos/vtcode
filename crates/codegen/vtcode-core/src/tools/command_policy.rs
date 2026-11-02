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
mod tests;

use anyhow::{Context, Result, ensure};
use regex::Regex;
use serde::{Deserialize, Serialize};

/// All lifecycle hook event names in canonical (snake_case) form, including
/// the deprecated aliases that `LifecycleHooksConfig::normalized()` folds into
/// `stop`. Used to classify which events a workspace-controlled config layer
/// defines.
pub(crate) const LIFECYCLE_HOOK_EVENTS: &[&str] = &[
    "session_start",
    "session_end",
    "subagent_start",
    "subagent_stop",
    "user_prompt_submit",
    "pre_tool_use",
    "post_tool_use",
    "permission_request",
    "pre_compact",
    "stop",
    "task_completion",
    "task_completed",
    "notification",
];

/// Top-level configuration for automation hooks and lifecycle events
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
pub struct HooksConfig {
    /// Configuration for lifecycle-based shell command execution
    #[serde(default)]
    pub lifecycle: LifecycleHooksConfig,
}

/// Configuration for hooks triggered during distinct agent lifecycle events.
/// Each event supports a list of groups with optional matchers.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
pub struct LifecycleHooksConfig {
    /// Suppress plain stdout from successful hooks unless they emit structured fields
    #[serde(default)]
    pub quiet_success_output: bool,

    /// Commands to run immediately when an agent session begins
    #[serde(default)]
    pub session_start: Vec<HookGroupConfig>,

    /// Commands to run when an agent session ends
    #[serde(default)]
    pub session_end: Vec<HookGroupConfig>,

    /// Commands to run when a delegated subagent starts
    #[serde(default)]
    pub subagent_start: Vec<HookGroupConfig>,

    /// Commands to run when a delegated subagent stops
    #[serde(default)]
    pub subagent_stop: Vec<HookGroupConfig>,

    /// Commands to run when the user submits a prompt (pre-processing)
    #[serde(default)]
    pub user_prompt_submit: Vec<HookGroupConfig>,

    /// Commands to run immediately before a tool is executed
    #[serde(default)]
    pub pre_tool_use: Vec<HookGroupConfig>,

    /// Commands to run immediately after a tool returns its output
    #[serde(default)]
    pub post_tool_use: Vec<HookGroupConfig>,

    /// Commands to run when Diapason is about to request interactive permission approval
    #[serde(default)]
    pub permission_request: Vec<HookGroupConfig>,

    /// Commands to run immediately before Diapason compacts conversation history
    #[serde(default)]
    pub pre_compact: Vec<HookGroupConfig>,

    /// Commands to run after Diapason produces a final answer but before the turn completes
    #[serde(default)]
    pub stop: Vec<HookGroupConfig>,

    /// Deprecated alias for `stop`
    #[serde(default)]
    pub task_completion: Vec<HookGroupConfig>,

    /// Deprecated alias for `stop`
    #[serde(default)]
    pub task_completed: Vec<HookGroupConfig>,

    /// Commands to run when Diapason emits a runtime notification event
    #[serde(default)]
    pub notification: Vec<HookGroupConfig>,
}

impl LifecycleHooksConfig {
    pub fn is_empty(&self) -> bool {
        self.session_start.is_empty()
            && self.session_end.is_empty()
            && self.subagent_start.is_empty()
            && self.subagent_stop.is_empty()
            && self.user_prompt_submit.is_empty()
            && self.pre_tool_use.is_empty()
            && self.post_tool_use.is_empty()
            && self.permission_request.is_empty()
            && self.pre_compact.is_empty()
            && self.stop.is_empty()
            && self.task_completion.is_empty()
            && self.task_completed.is_empty()
            && self.notification.is_empty()
    }

    pub fn normalized(&self) -> Self {
        let mut normalized = self.clone();
        normalized.stop.extend(self.task_completion.clone());
        normalized.stop.extend(self.task_completed.clone());
        normalized.task_completion.clear();
        normalized.task_completed.clear();
        normalized
    }

    /// Return the groups stored under a canonical snake_case event name
    /// (the literal field; deprecated aliases keep their own fields here and
    /// are folded into `stop` only by `normalized()`).
    pub(crate) fn groups_for_event(&self, event: &str) -> &[HookGroupConfig] {
        match event {
            "session_start" => &self.session_start,
            "session_end" => &self.session_end,
            "subagent_start" => &self.subagent_start,
            "subagent_stop" => &self.subagent_stop,
            "user_prompt_submit" => &self.user_prompt_submit,
            "pre_tool_use" => &self.pre_tool_use,
            "post_tool_use" => &self.post_tool_use,
            "permission_request" => &self.permission_request,
            "pre_compact" => &self.pre_compact,
            "stop" => &self.stop,
            "task_completion" => &self.task_completion,
            "task_completed" => &self.task_completed,
            "notification" => &self.notification,
            _ => &[],
        }
    }
}

/// A group of hooks sharing a common execution matcher
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
pub struct HookGroupConfig {
    /// Optional regex matcher to filter when this group runs.
    /// Matched against context strings (e.g. tool name, project path).
    #[serde(default)]
    pub matcher: Option<String>,

    /// List of hook commands to execute sequentially in this group
    #[serde(default)]
    pub hooks: Vec<HookCommandConfig>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum HookCommandKind {
    #[default]
    Command,
    /// Catch-all for unknown hook kinds added by future versions.
    #[serde(other)]
    Unknown,
}

/// Configuration for a single shell command hook
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
pub struct HookCommandConfig {
    /// Type of hook command (currently only 'command' is supported)
    #[serde(default)]
    #[serde(rename = "type")]
    pub kind: HookCommandKind,

    /// The shell command string to execute
    #[serde(default)]
    pub command: String,

    /// Optional execution timeout in seconds
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

impl HooksConfig {
    pub fn validate(&self) -> Result<()> {
        self.lifecycle.validate().context("Invalid lifecycle hooks configuration")
    }
}

/// A single lifecycle hook command whose effective configuration origin is a
/// workspace-controlled layer (workspace-root `config.toml`, the workspace
/// `.diapason/config.toml` fallback, or a project profile inside the workspace).
///
/// Workspace-controlled configuration is attacker-influenceable in an
/// untrusted repository, so these commands must never execute until the user
/// explicitly approves the exact command set for this workspace.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceHookCommand {
    /// Lifecycle event name in canonical snake_case form (e.g. `session_start`).
    /// Deprecated aliases (`task_completion`, `task_completed`) are recorded as
    /// `stop` because `LifecycleHooksConfig::normalized()` folds them there.
    pub event: String,
    /// Optional regex matcher of the containing group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// The shell command string executed via `sh -c`.
    pub command: String,
    /// Optional execution timeout in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

/// Snapshot of all lifecycle hook commands sourced from workspace-controlled
/// configuration layers, captured at configuration load time.
///
/// The presence of any workspace-controlled hook command marks the session's
/// lifecycle engine as gated: no lifecycle hook runs until the user explicitly
/// approves the exact command set the engine will execute, bound to a digest
/// of that set. Any change to the commands invalidates a previously granted
/// approval, so a repository update cannot silently swap in a new command
/// under an old approval.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceLifecycleHooks {
    /// Workspace-controlled lifecycle hook commands in canonical event order.
    pub commands: Vec<WorkspaceHookCommand>,
}

impl WorkspaceLifecycleHooks {
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

impl LifecycleHooksConfig {
    fn validate(&self) -> Result<()> {
        validate_groups(&self.session_start, "session_start")?;
        validate_groups(&self.session_end, "session_end")?;
        validate_groups(&self.subagent_start, "subagent_start")?;
        validate_groups(&self.subagent_stop, "subagent_stop")?;
        validate_groups(&self.user_prompt_submit, "user_prompt_submit")?;
        validate_groups(&self.pre_tool_use, "pre_tool_use")?;
        validate_groups(&self.post_tool_use, "post_tool_use")?;
        validate_groups(&self.permission_request, "permission_request")?;
        validate_groups(&self.pre_compact, "pre_compact")?;
        validate_groups(&self.stop, "stop")?;
        validate_groups(&self.task_completion, "task_completion")?;
        validate_groups(&self.task_completed, "task_completed")?;
        validate_groups(&self.notification, "notification")?;
        Ok(())
    }
}

fn validate_groups(groups: &[HookGroupConfig], context_name: &str) -> Result<()> {
    for (index, group) in groups.iter().enumerate() {
        if let Some(pattern) = group.matcher.as_ref() {
            validate_matcher(pattern)
                .with_context(|| format!("Invalid matcher in hooks.{context_name}[{index}] -> matcher"))?;
        }

        ensure!(!group.hooks.is_empty(), "hooks.{context_name}[{index}] must define at least one hook command");

        for (hook_index, hook) in group.hooks.iter().enumerate() {
            ensure!(
                matches!(hook.kind, HookCommandKind::Command),
                "hooks.{context_name}[{index}].hooks[{hook_index}] has unsupported type"
            );

            ensure!(
                !hook.command.trim().is_empty(),
                "hooks.{context_name}[{index}].hooks[{hook_index}] must specify a command"
            );

            if let Some(timeout) = hook.timeout_seconds {
                ensure!(
                    timeout > 0,
                    "hooks.{context_name}[{index}].hooks[{hook_index}].timeout_seconds must be positive"
                );
            }
        }
    }

    Ok(())
}

fn validate_matcher(pattern: &str) -> Result<()> {
    let trimmed = pattern.trim();
    if trimmed.is_empty() || trimmed == "*" {
        return Ok(());
    }

    let regex_pattern = format!("^(?:{trimmed})$");
    Regex::new(&regex_pattern).with_context(|| format!("failed to compile lifecycle hook matcher: {pattern}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_group() -> HookGroupConfig {
        HookGroupConfig {
            matcher: None,
            hooks: vec![HookCommandConfig {
                kind: HookCommandKind::Command,
                command: "echo ok".to_string(),
                timeout_seconds: None,
            }],
        }
    }

    #[test]
    fn permission_request_and_stop_validate() {
        let config = LifecycleHooksConfig {
            permission_request: vec![sample_group()],
            stop: vec![sample_group()],
            ..Default::default()
        };

        config.validate().expect("hooks validate");
    }

    #[test]
    fn task_aliases_normalize_into_stop() {
        let config = LifecycleHooksConfig {
            task_completion: vec![sample_group()],
            task_completed: vec![sample_group()],
            ..Default::default()
        };

        let normalized = config.normalized();
        assert_eq!(normalized.stop.len(), 2);
        assert!(normalized.task_completion.is_empty());
        assert!(normalized.task_completed.is_empty());
    }
}

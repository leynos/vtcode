//! Shared system-prompt catalogue for delegated child agents.

use vtcode_config::SubagentSpec;

const CATALOGUE_HEADER: &str = "## Subagents\nDelegated child agents available in this session. Treat the main thread as the controller: keep the next blocking step local, and delegate only bounded independent work. Read-only agents may be used proactively when their description matches; write-capable agents require explicit delegation.\nUsers can explicitly target one with natural language or an `@agent-<name>` mention.\nIf the user explicitly selects a subagent for the task, use the `agent` tool with `action=spawn` to delegate to that subagent instead of handling the task on the main thread. Join child results back into the parent flow before you depend on them.";

/// Model-visible summary of one runnable delegated child agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentPromptEntry {
    pub name: String,
    pub description: String,
    pub read_only: bool,
}

impl From<&SubagentSpec> for SubagentPromptEntry {
    fn from(spec: &SubagentSpec) -> Self {
        Self {
            name: spec.name.clone(),
            description: spec.description.clone(),
            read_only: spec.is_read_only(),
        }
    }
}

/// Render the delegated-agent catalogue within the caller's remaining prompt
/// budget.
///
/// Small catalogues retain full descriptions. Larger catalogues retain every
/// agent name while summarising descriptions, so the model can still supply a
/// valid `agent_type` to the collaboration tool.
#[must_use]
pub fn render_subagent_section(subagents: &[SubagentPromptEntry], max_chars: usize) -> Option<String> {
    if subagents.is_empty() {
        return None;
    }

    if subagents.len() > 3 {
        return Some(build_summarized_subagent_section(subagents));
    }

    let full = build_full_subagent_section(subagents);
    if full.len() <= max_chars {
        Some(full)
    } else {
        Some(budget_subagent_section(subagents, max_chars))
    }
}

fn build_full_subagent_section(subagents: &[SubagentPromptEntry]) -> String {
    let mut lines = Vec::with_capacity(4 + subagents.len());
    lines.extend(CATALOGUE_HEADER.lines().map(str::to_string));
    for subagent in subagents {
        let suffix = if subagent.read_only {
            " Read-only."
        } else {
            " Explicit delegation only."
        };
        lines.push(format!("- {}: {}{suffix}", subagent.name, subagent.description));
    }
    lines.join("\n")
}

fn build_summarized_subagent_section(subagents: &[SubagentPromptEntry]) -> String {
    let count = subagents.len();
    let read_only = subagents.iter().filter(|entry| entry.read_only).count();
    let writable = count - read_only;
    let names = subagents
        .iter()
        .map(|entry| format!("`{}`", entry.name))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "## Subagents\n{count} subagents available ({read_only} read-only, {writable} writable): {names}. Use the `agent` tool with `action=spawn` and one of these names as `agent_type` to delegate."
    )
}

fn budget_subagent_section(subagents: &[SubagentPromptEntry], max_chars: usize) -> String {
    let mut lines = CATALOGUE_HEADER.lines().map(str::to_string).collect::<Vec<_>>();
    let mut used = lines.iter().map(String::len).sum::<usize>() + lines.len().saturating_sub(1);

    for (index, subagent) in subagents.iter().enumerate() {
        let suffix = if subagent.read_only {
            " Read-only."
        } else {
            " Explicit delegation only."
        };
        let entry = format!("- {}: {}{suffix}", subagent.name, subagent.description);
        if used.saturating_add(entry.len() + 1) > max_chars {
            lines.push(format!("- ... ({} more agents truncated)", subagents.len() - index));
            break;
        }
        used = used.saturating_add(entry.len() + 1);
        lines.push(entry);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{SubagentPromptEntry, render_subagent_section};

    fn entry(name: &str, description: &str, read_only: bool) -> SubagentPromptEntry {
        SubagentPromptEntry {
            name: name.to_string(),
            description: description.to_string(),
            read_only,
        }
    }

    #[test]
    fn small_catalogue_includes_descriptions_and_access_modes() {
        let entries = [
            entry("explorer", "Read-only repo explorer", true),
            entry("builder", "Write-capable implementation agent", false),
        ];

        let rendered = render_subagent_section(&entries, usize::MAX).expect("catalogue");

        assert!(rendered.contains("explorer: Read-only repo explorer Read-only."));
        assert!(rendered.contains("builder: Write-capable implementation agent Explicit delegation only."));
        assert!(rendered.contains("`agent` tool with `action=spawn`"));
    }

    #[test]
    fn large_catalogue_retains_every_runnable_agent_name() {
        let entries = (0..5)
            .map(|index| entry(&format!("agent-{index}"), &format!("Description {index}"), index % 2 == 0))
            .collect::<Vec<_>>();

        let rendered = render_subagent_section(&entries, usize::MAX).expect("catalogue");

        assert!(rendered.contains("5 subagents available"));
        for index in 0..5 {
            assert!(rendered.contains(&format!("`agent-{index}`")));
        }
        assert!(!rendered.contains("Description 0"));
    }

    #[test]
    fn small_catalogue_uses_truncation_when_descriptions_exceed_budget() {
        let entries = [
            entry("first", &"long ".repeat(100), true),
            entry("second", &"long ".repeat(100), false),
        ];

        let rendered = render_subagent_section(&entries, 600).expect("catalogue");

        assert!(rendered.contains("more agents truncated"));
        assert!(!rendered.contains("second: long"));
    }

    #[test]
    fn empty_catalogue_is_omitted() {
        assert_eq!(render_subagent_section(&[], usize::MAX), None);
    }
}

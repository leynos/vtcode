//! ACP-specific system-prompt assembly.

use std::path::Path;

use vtcode_config::SubagentSpec;
use vtcode_core::config::VTCodeConfig;
use vtcode_core::config::constants::prompt_budget as prompt_budget_constants;
use vtcode_core::prompts::system::{SystemPromptReport, apply_output_style, compose_system_instruction_with_report};
use vtcode_core::prompts::{PromptContext, SubagentPromptEntry, render_subagent_section};

pub(crate) async fn build_acp_system_prompt(
    workspace: &Path,
    vt_cfg: Option<&VTCodeConfig>,
    available_tools: &[String],
    available_subagents: &[SubagentSpec],
) -> String {
    let mut prompt_context = PromptContext::from_workspace_tools(workspace, available_tools.iter().cloned());
    prompt_context.load_available_skills_async().await;

    let (prompt, report) = compose_system_instruction_with_report(workspace, vt_cfg, Some(&prompt_context)).await;
    let mut prompt = apply_output_style(prompt, vt_cfg, workspace).await;
    let max_tokens = vt_cfg
        .map(|config| config.agent.max_system_prompt_tokens)
        .unwrap_or(prompt_budget_constants::DEFAULT_MAX_SYSTEM_PROMPT_TOKENS);
    let styled_report = SystemPromptReport::measure(&prompt, max_tokens);
    let remaining_chars = usize::try_from(
        max_tokens
            .saturating_sub(styled_report.token_estimate.max(report.token_estimate))
            .saturating_mul(4),
    )
    .unwrap_or(usize::MAX);
    let entries = available_subagents
        .iter()
        .filter(|spec| spec.is_subagent())
        .map(SubagentPromptEntry::from)
        .collect::<Vec<_>>();

    if let Some(section) = render_subagent_section(&entries, remaining_chars) {
        prompt.push_str("\n\n");
        prompt.push_str(&section);
    }
    prompt
}

#[cfg(test)]
mod tests {
    use std::fs;

    use assert_fs::TempDir;
    use vtcode_config::{SubagentDiscoveryInput, discover_subagents};
    use vtcode_core::config::VTCodeConfig;

    use super::build_acp_system_prompt;

    #[tokio::test]
    async fn acp_prompt_catalogues_workspace_skills_and_active_subagents() {
        let workspace = TempDir::new().expect("workspace");
        let skill_dir = workspace.path().join(".agents/skills/000-acp-prompt-marker-skill");
        fs::create_dir_all(&skill_dir).expect("create skill directory");
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: 000-acp-prompt-marker-skill\ndescription: ACP prompt marker skill\n---\n# Marker\n",
        )
        .expect("write skill");

        let agent_dir = workspace.path().join(".claude/agents");
        fs::create_dir_all(&agent_dir).expect("create agent directory");
        fs::write(
            agent_dir.join("acp-prompt-marker-agent.md"),
            "---\nname: acp-prompt-marker-agent\ndescription: ACP prompt marker agent\nmode: subagent\npermissions:\n  default: deny\n---\nInspect without editing.\n",
        )
        .expect("write agent");

        let mut discovery_input = SubagentDiscoveryInput::new(workspace.path().to_path_buf());
        discovery_input.include_user_agents = false;
        let discovered = discover_subagents(&discovery_input).expect("discover subagents");
        let mut config = VTCodeConfig::default();
        config.skills.bundled.enabled = false;

        let prompt = build_acp_system_prompt(workspace.path(), Some(&config), &[], &discovered.effective).await;

        assert!(prompt.contains("## Skills"));
        assert!(prompt.contains("000-acp-prompt-marker-skill: ACP prompt marker skill"));
        assert!(prompt.contains("## Subagents"));
        assert!(prompt.contains("`acp-prompt-marker-agent`"));
        assert!(prompt.contains("`agent` tool with `action=spawn`"));
    }
}

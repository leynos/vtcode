//! Active-agent runtime-state rendering.
//!
//! This module is deliberately kept separate from prompt assembly: runtime
//! state is appended by the request builder after the catalogue has been
//! validated, while the assembly module owns section ordering and recovery.

use std::fmt::Write as _;

use vtcode_core::ActivePrimaryAgent;
use vtcode_core::config::types::ReasoningEffortLevel;
use vtcode_core::core::agent::harness_kernel::SessionToolCatalogueSnapshot;
use vtcode_core::prompts::PromptContext;
use vtcode_core::subagents::load_primary_memory_appendix_async;

use super::snapshot::TurnRequestSnapshot;
use crate::agent::runloop::unified::turn::context::TurnProcessingContext;

pub(super) async fn render_primary_agent_runtime_context(
    ctx: &TurnProcessingContext<'_>,
    turn_snapshot: &TurnRequestSnapshot,
    tool_snapshot: &SessionToolCatalogueSnapshot,
    agent: &ActivePrimaryAgent,
    reasoning_effort: Option<ReasoningEffortLevel>,
    agent_prompt_context: Option<&PromptContext>,
) -> String {
    let mut buf = String::with_capacity(1024);
    let _ = writeln!(buf, "## Active Primary Agent Runtime State");
    let _ = writeln!(buf, "- Active agent: {}", agent.display_name);
    let _ = writeln!(buf, "- Spec name: {}", agent.identity.name);
    let _ = writeln!(buf, "- Request model: {}", turn_snapshot.active_model);
    if let Some(model) = agent
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty() && !model.eq_ignore_ascii_case("inherit"))
    {
        let _ = writeln!(buf, "- Agent model: {model}");
    }
    if let Some(effort) = reasoning_effort {
        let _ = writeln!(buf, "- Request reasoning effort: {}", effort.as_str());
    }
    if let Some(raw_effort) = agent.reasoning_effort {
        let _ = writeln!(buf, "- Agent reasoning effort: {}", raw_effort.as_str());
    }
    let _ = writeln!(
        buf,
        "- Session state: planning_workflow={}, full_auto={}",
        turn_snapshot.planning_active, turn_snapshot.full_auto
    );
    let _ = writeln!(buf, "- Effective request tools: {}", render_tool_names(tool_snapshot));
    if let Some(tools) = agent.tools.as_ref().filter(|tools| !tools.is_empty()) {
        let _ = writeln!(buf, "- Primary-agent tool allow-list: {}", tools.join(", "));
    }
    if !agent.disallowed_tools.is_empty() {
        let _ = writeln!(buf, "- Primary-agent disallowed tools: {}", agent.disallowed_tools.join(", "));
    }
    if !agent.skills.is_empty() {
        if let Some(prompt_context) = agent_prompt_context {
            if prompt_context.available_skill_metadata.is_empty() {
                let _ = writeln!(buf, "- Active primary skills: {}", agent.skills.join(", "));
            } else {
                let mut names = prompt_context
                    .available_skill_metadata
                    .iter()
                    .map(|skill| skill.name.as_str())
                    .collect::<Vec<_>>();
                names.sort_unstable();
                let _ = writeln!(buf, "- Active primary skills: {}", names.join(", "));
            }
        } else {
            let _ = writeln!(buf, "- Active primary skills: {}", agent.skills.join(", "));
        }
    }
    let _ = writeln!(buf, "### Instructions");
    let _ = writeln!(buf, "{}", agent.instructions.trim());
    if let Some(memory_appendix) = active_primary_agent_memory_appendix(ctx, agent).await {
        let _ = writeln!(buf, "### Memory Appendix");
        let _ = writeln!(buf, "{memory_appendix}");
    }
    buf
}

async fn active_primary_agent_memory_appendix(
    ctx: &TurnProcessingContext<'_>,
    agent: &ActivePrimaryAgent,
) -> Option<String> {
    match load_primary_memory_appendix_async(ctx.config.workspace.as_path(), agent.identity.name.as_str(), agent.memory)
        .await
    {
        Ok(appendix) => appendix,
        Err(err) => {
            tracing::warn!(
                agent_name = %agent.identity.name,
                error = %err,
                "Failed to load active primary-agent memory appendix"
            );
            None
        }
    }
}

fn render_tool_names(tool_snapshot: &SessionToolCatalogueSnapshot) -> String {
    let Some(tools) = tool_snapshot.snapshot.as_deref() else {
        return "none".to_string();
    };
    if tools.is_empty() {
        return "none".to_string();
    }

    let mut result = String::new();
    for (index, tool) in tools.iter().enumerate() {
        if index > 0 {
            result.push_str(", ");
        }
        result.push_str(
            tool.function
                .as_ref()
                .map(|function| function.name.as_str())
                .unwrap_or(tool.tool_type.as_str()),
        );
    }
    result
}

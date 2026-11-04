//! Prompt environment and interaction rendering helpers.

use crate::config::DiapasonConfig;
use crate::prompts::context::PromptContext;
use crate::prompts::temporal::generate_temporal_context;

/// Assemble the environment addenda section for the system prompt.
pub fn render_environment_addenda(
    diapason_config: Option<&DiapasonConfig>,
    prompt_context: Option<&PromptContext>,
) -> Option<String> {
    let mut lines = Vec::new();

    if let Some(ctx) = prompt_context
        && !ctx.languages.is_empty()
    {
        lines.push(format!("- Languages: {}. Match structural-search `lang` when needed.", ctx.languages.join(", ")));
    }

    if let Some(cfg) = diapason_config {
        if let Some(interaction_line) = render_interaction_addendum(cfg) {
            lines.push(interaction_line);
        }

        if cfg.mcp.enabled {
            lines.push(
                "- Sources: prefer MCP (including Agent Plugin servers) before external fetches when available."
                    .to_string(),
            );
        }

        // The system-prompt cache freezes this value at segment/session start.
        // Do not move temporal context into a per-turn runtime appendix: doing
        // so changes the cache prefix on every request.
        if cfg.agent.include_temporal_context {
            lines.push(
                generate_temporal_context(cfg.agent.temporal_context_use_utc)
                    .trim()
                    .replacen("Current date and time", "- Time", 1)
                    .to_string(),
            );
        }

        if cfg.agent.include_working_directory
            && let Some(ctx) = prompt_context
            && let Some(cwd) = &ctx.current_directory
        {
            lines.push(format!("- Working directory: {}", cwd.display()));
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(format!("## Environment\n{}", lines.join("\n")))
    }
}

/// Render the interaction addendum line based on HITL and ask_questions config.
fn render_interaction_addendum(cfg: &DiapasonConfig) -> Option<String> {
    match cfg.chat.ask_questions.enabled {
        true => None,
        false => Some(
            "- Interaction: no `request_user_input`; make reasonable assumptions unless Planning workflow needs follow-up."
                .to_string(),
        ),
    }
}

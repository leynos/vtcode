//! Static prompt text, constants, and cached static prompt builders.

use std::sync::OnceLock;

use crate::config::types::SystemPromptMode;
use crate::prompts::runtime_guidance::{RUNTIME_GUIDANCE_MAX_ESTIMATED_TOKENS, runtime_guidance_section};
use crate::prompts::system::{
    CONTRACT_HEADER, DEFAULT_OPERATING_PROFILE_DELTA, DEFAULT_SPECIFIC_LINES, LIGHTWEIGHT_OPERATING_PROFILE_DELTA,
    MINIMAL_OPERATING_PROFILE_DELTA, MINIMAL_SPECIFIC_LINES, PROMPT_INTRO, PROMPT_ROLE_PARAGRAPH, PROMPT_TITLE,
    SHARED_CONTRACT_LINES, SPECIALIZED_OPERATING_PROFILE_DELTA,
};

/// Agent identity labels for the system prompt.
/// Maps agent names to human-readable identity strings that combine Diapason
/// with the active agent mode, so the LLM knows its role.
pub fn agent_identity_label(agent_name: &str) -> String {
    match agent_name {
        "build" => "Diapason (Build mode)".to_string(),
        "auto" => "Diapason (Auto mode)".to_string(),
        "duck" => "Diapason (Duck mode)".to_string(),
        "plan" => "Diapason (Plan mode)".to_string(),
        "explorer" => "Diapason (Explorer mode)".to_string(),
        "worker" => "Diapason (Worker mode)".to_string(),
        other => format!("Diapason ({other})"),
    }
}

static DEFAULT_SYSTEM_PROMPT: OnceLock<String> = OnceLock::new();
static MINIMAL_SYSTEM_PROMPT: OnceLock<String> = OnceLock::new();
static DEFAULT_LIGHTWEIGHT_PROMPT: OnceLock<String> = OnceLock::new();
static DEFAULT_SPECIALIZED_PROMPT: OnceLock<String> = OnceLock::new();

fn append_runtime_guidance(prompt: &mut String) {
    let guidance = runtime_guidance_section();
    assert!(guidance.len().div_ceil(4) <= RUNTIME_GUIDANCE_MAX_ESTIMATED_TOKENS);
    prompt.push_str(guidance);
    prompt.push('\n');
}

pub fn default_system_prompt() -> &'static str {
    static_profile_prompt(SystemPromptMode::Default)
}

pub fn minimal_system_prompt() -> &'static str {
    static_profile_prompt(SystemPromptMode::Minimal)
}

pub fn default_lightweight_prompt() -> &'static str {
    static_profile_prompt(SystemPromptMode::Lightweight)
}

pub fn specialized_system_prompt() -> &'static str {
    static_profile_prompt(SystemPromptMode::Specialized)
}

pub fn minimal_instruction_text() -> String {
    minimal_system_prompt().to_string()
}

pub fn lightweight_instruction_text() -> String {
    default_lightweight_prompt().to_string()
}

pub fn specialized_instruction_text() -> String {
    specialized_system_prompt().to_string()
}

pub fn static_profile_prompt(prompt_mode: SystemPromptMode) -> &'static str {
    match prompt_mode {
        SystemPromptMode::Default => DEFAULT_SYSTEM_PROMPT.get_or_init(|| {
            let mut prompt = String::new();
            prompt.push_str(PROMPT_TITLE);
            prompt.push_str("\n\n");
            prompt.push_str(PROMPT_INTRO);
            prompt.push_str("\n\n");
            prompt.push_str(PROMPT_ROLE_PARAGRAPH);
            prompt.push_str("\n\n");
            append_runtime_guidance(&mut prompt);
            prompt.push_str(CONTRACT_HEADER);
            prompt.push_str("\n\n");
            for line in SHARED_CONTRACT_LINES {
                prompt.push_str("- ");
                prompt.push_str(line);
                prompt.push('\n');
            }
            prompt.pop();
            prompt.push('\n');
            for line in DEFAULT_SPECIFIC_LINES {
                prompt.push_str("- ");
                prompt.push_str(line);
                prompt.push('\n');
            }
            prompt.pop();
            prompt.push('\n');
            prompt.push('\n');
            prompt.push_str(DEFAULT_OPERATING_PROFILE_DELTA);
            prompt
        }),
        SystemPromptMode::Minimal => MINIMAL_SYSTEM_PROMPT.get_or_init(|| {
            let mut prompt = String::new();
            prompt.push_str(PROMPT_TITLE);
            prompt.push_str("\n\n");
            prompt.push_str(PROMPT_INTRO);
            prompt.push_str("\n\n");
            append_runtime_guidance(&mut prompt);
            prompt.push_str(CONTRACT_HEADER);
            prompt.push_str("\n\n");
            for line in SHARED_CONTRACT_LINES {
                prompt.push_str("- ");
                prompt.push_str(line);
                prompt.push('\n');
            }
            prompt.pop();
            prompt.push('\n');
            for line in MINIMAL_SPECIFIC_LINES {
                prompt.push_str("- ");
                prompt.push_str(line);
                prompt.push('\n');
            }
            prompt.pop();
            prompt.push('\n');
            prompt.push('\n');
            prompt.push_str(MINIMAL_OPERATING_PROFILE_DELTA);
            prompt
        }),
        SystemPromptMode::Lightweight => DEFAULT_LIGHTWEIGHT_PROMPT.get_or_init(|| {
            let mut prompt = String::new();
            prompt.push_str(PROMPT_TITLE);
            prompt.push_str("\n\n");
            prompt.push_str(PROMPT_INTRO);
            prompt.push_str("\n\n");
            append_runtime_guidance(&mut prompt);
            prompt.push_str(CONTRACT_HEADER);
            prompt.push_str("\n\n");
            for line in SHARED_CONTRACT_LINES {
                prompt.push_str("- ");
                prompt.push_str(line);
                prompt.push('\n');
            }
            prompt.pop();
            prompt.push('\n');
            for line in DEFAULT_SPECIFIC_LINES {
                prompt.push_str("- ");
                prompt.push_str(line);
                prompt.push('\n');
            }
            prompt.pop();
            prompt.push('\n');
            prompt.push('\n');
            prompt.push_str(LIGHTWEIGHT_OPERATING_PROFILE_DELTA);
            prompt
        }),
        SystemPromptMode::Specialized => DEFAULT_SPECIALIZED_PROMPT.get_or_init(|| {
            let mut prompt = String::new();
            prompt.push_str(PROMPT_TITLE);
            prompt.push_str("\n\n");
            prompt.push_str(PROMPT_INTRO);
            prompt.push_str("\n\n");
            prompt.push_str(PROMPT_ROLE_PARAGRAPH);
            prompt.push_str("\n\n");
            append_runtime_guidance(&mut prompt);
            prompt.push_str(CONTRACT_HEADER);
            prompt.push_str("\n\n");
            for line in SHARED_CONTRACT_LINES {
                prompt.push_str("- ");
                prompt.push_str(line);
                prompt.push('\n');
            }
            prompt.pop();
            prompt.push('\n');
            for line in DEFAULT_SPECIFIC_LINES {
                prompt.push_str("- ");
                prompt.push_str(line);
                prompt.push('\n');
            }
            prompt.pop();
            prompt.push('\n');
            prompt.push('\n');
            prompt.push_str(SPECIALIZED_OPERATING_PROFILE_DELTA);
            prompt
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::constants::prompt_budget;
    use crate::prompts::runtime_guidance::RUNTIME_GUIDANCE_SECTION;

    #[test]
    fn static_prompts_include_required_sections() {
        let modes = [
            (SystemPromptMode::Default, default_system_prompt()),
            (SystemPromptMode::Minimal, minimal_system_prompt()),
            (SystemPromptMode::Lightweight, default_lightweight_prompt()),
            (SystemPromptMode::Specialized, specialized_system_prompt()),
        ];
        for (mode, prompt) in modes {
            assert!(prompt.contains(PROMPT_TITLE), "{mode:?} missing title");
            assert!(prompt.contains(CONTRACT_HEADER), "{mode:?} missing contract");
            assert!(prompt.contains("AGENTS.md"), "{mode:?} missing AGENTS.md ref");
            assert_eq!(
                prompt.matches(RUNTIME_GUIDANCE_SECTION).count(),
                1,
                "{mode:?} should include runtime guidance exactly once"
            );
            assert!(
                prompt.find(RUNTIME_GUIDANCE_SECTION) < prompt.find(CONTRACT_HEADER),
                "{mode:?} should place runtime guidance before the contract"
            );
            assert!(
                (prompt.len().div_ceil(4) as u64) <= prompt_budget::DEFAULT_MAX_SYSTEM_PROMPT_TOKENS,
                "{mode:?} static prompt should fit the default budget"
            );
        }
    }

    #[test]
    fn static_prompts_do_not_embed_maintainer_files() {
        let prompts = [
            default_system_prompt(),
            minimal_system_prompt(),
            default_lightweight_prompt(),
            specialized_system_prompt(),
        ];
        let maintainer_only_markers = [
            "Keep this file concise and under 150 lines",
            "diapason-exec-events::ThreadEvent",
            "RUSTFLAGS: \"-D warnings\"",
            "Cargo workspace, ~30 crates",
            "Root summary",
        ];

        for prompt in prompts {
            for marker in maintainer_only_markers {
                assert!(!prompt.contains(marker), "static prompt unexpectedly contains {marker:?}");
            }
        }
    }
}

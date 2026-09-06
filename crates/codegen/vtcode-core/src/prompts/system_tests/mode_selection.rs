//! Prompt-mode selection and base token-count regressions.

use super::*;

#[tokio::test]
async fn test_minimal_mode_selection() {
    let mut config = VTCodeConfig::default();
    config.agent.system_prompt_mode = SystemPromptMode::Minimal;
    // Disable enhancements for base prompt size testing
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;
    config.agent.instruction_max_bytes = 0;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    // Minimal prompt should remain compact and deterministic without AGENTS.md injection
    assert!(result.len() < 2800, "Minimal mode should produce <2.8K chars (was {} chars)", result.len());
    assert!(result.contains("VT Code") || result.contains("VT Code"), "Should contain VT Code identifier");
}

#[tokio::test]
async fn test_default_prompt_selection() {
    let mut config = VTCodeConfig::default();
    config.agent.system_prompt_mode = SystemPromptMode::Default;
    // Disable enhancements for base prompt size testing
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;
    config.agent.instruction_max_bytes = 0;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(
        result.len() <= 3400,
        "Default mode should stay sparse with runtime guidance (<=3.4K chars, was {} chars)",
        result.len()
    );
    assert!(result.contains("`exec_command`, `write_stdin`, and `apply_patch`"));
    assert!(result.contains("## Shell Profile"));
    assert!(!result.contains("task_tracker"));
    assert!(!result.contains("@file"));
    assert!(result.contains("Planning workflow"));
}

#[tokio::test]
async fn test_lightweight_mode_selection() {
    let mut config = VTCodeConfig::default();
    config.agent.system_prompt_mode = SystemPromptMode::Lightweight;
    // Disable enhancements for base prompt size testing
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;
    config.agent.instruction_max_bytes = 0;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(result.len() > 100, "Lightweight should be >100 chars");
    assert!(
        result.len() < 2700,
        "Lightweight should be compact with runtime guidance (<2.7K chars, was {} chars)",
        result.len()
    );
    assert!(result.contains("task_tracker"));
    assert!(!result.contains("@file"));
    assert!(result.contains("Act and verify in one thread"));
}

#[tokio::test]
async fn test_lightweight_mode_skips_structured_reasoning_by_default() {
    let mut config = VTCodeConfig::default();
    config.agent.system_prompt_mode = SystemPromptMode::Lightweight;
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;
    config.agent.instruction_max_bytes = 0;
    config.agent.include_structured_reasoning_tags = None;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(
        !result.contains("## Structured Reasoning"),
        "Lightweight mode should omit structured reasoning by default"
    );
}

#[tokio::test]
async fn test_lightweight_mode_allows_explicit_structured_reasoning() {
    let mut config = VTCodeConfig::default();
    config.agent.system_prompt_mode = SystemPromptMode::Lightweight;
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;
    config.agent.instruction_max_bytes = 0;
    config.agent.include_structured_reasoning_tags = Some(true);

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(
        result.contains("## Structured Reasoning"),
        "Lightweight mode should include structured reasoning when explicitly enabled"
    );
    assert!(result.contains("<reasoning_plan>"));
    assert!(!result.contains("`<plan>` steps"), "<plan> is reserved for approval artefacts");
}

#[tokio::test]
async fn test_default_prompt_omits_structured_reasoning_by_default() {
    let mut config = VTCodeConfig::default();
    config.agent.system_prompt_mode = SystemPromptMode::Default;
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;
    config.agent.instruction_max_bytes = 0;
    config.agent.include_structured_reasoning_tags = None;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(
        !result.contains("## Structured Reasoning"),
        "Default mode should omit structured reasoning by default"
    );
}

#[tokio::test]
async fn test_specialized_mode_selection() {
    let mut config = VTCodeConfig::default();
    config.agent.system_prompt_mode = SystemPromptMode::Specialized;
    // Disable enhancements for base prompt size testing
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;
    config.agent.instruction_max_bytes = 0;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(
        result.len() <= 3500,
        "Specialized should stay sparse with runtime guidance (<=3.5K chars, was {} chars)",
        result.len()
    );
    assert!(result.contains("task_tracker"));
    assert!(result.contains("<proposed_plan>"));
    assert!(result.contains("ARCHITECTURAL_INVARIANTS"));
}

#[test]
fn test_prompt_mode_enum_parsing() {
    assert_eq!(SystemPromptMode::parse("minimal"), Some(SystemPromptMode::Minimal));
    assert_eq!(SystemPromptMode::parse("LIGHTWEIGHT"), Some(SystemPromptMode::Lightweight));
    assert_eq!(SystemPromptMode::parse("Default"), Some(SystemPromptMode::Default));
    assert_eq!(SystemPromptMode::parse("specialized"), Some(SystemPromptMode::Specialized));
    assert_eq!(SystemPromptMode::parse("invalid"), None);
}

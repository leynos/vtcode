//! Static prompt guidance and operating-profile regressions.

use super::*;

#[test]
fn test_prompt_text_avoids_hardcoded_loop_thresholds() {
    let specialized_prompt = specialized_instruction_text();
    assert!(!default_system_prompt().contains("stuck twice"));
    assert!(!minimal_system_prompt().contains("stuck twice"));
    assert!(!specialized_prompt.contains("stuck twice"));
    assert!(!specialized_prompt.contains("10+ calls without progress"));
    assert!(!specialized_prompt.contains("Same tool+params twice"));
}

#[test]
fn test_harness_awareness_in_prompts() {
    assert!(default_system_prompt().contains("AGENTS.md"), "Default prompt should reference AGENTS.md as map");
    assert!(
        specialized_instruction_text().contains("ARCHITECTURAL_INVARIANTS"),
        "Specialized prompt should reference architectural invariants"
    );
    assert!(minimal_system_prompt().contains("AGENTS.md"), "Minimal prompt should still reference AGENTS.md");
}

#[test]
fn test_prompts_reject_guessing_when_context_is_missing() {
    assert!(default_system_prompt().contains("do not guess"), "Default prompt should reject guessing");
    assert!(specialized_instruction_text().contains("do not guess"), "Specialized prompt should reject guessing");
    assert!(minimal_system_prompt().contains("do not guess"), "Minimal prompt should still reject guessing");
}

#[test]
fn test_prompts_include_compaction_preservation_contract() {
    assert!(
        default_system_prompt().contains("touched files"),
        "Default prompt should preserve touched files across compaction"
    );
    assert!(
        default_system_prompt().contains("decisions across compaction"),
        "Default prompt should preserve decision rationale across compaction"
    );
    assert!(
        default_system_prompt().contains("tracker state"),
        "Default prompt should preserve tracker state across compaction"
    );
    assert!(
        default_system_prompt().contains("verification status"),
        "Default prompt should preserve verification status across compaction"
    );
    assert!(
        minimal_system_prompt().contains("touched files"),
        "Minimal prompt should preserve touched files across compaction"
    );
}

#[test]
fn test_default_prompt_stays_lean_but_complete() {
    let prompt = default_system_prompt();

    assert!(prompt.contains("## Contract"), "Default prompt should include the lean contract section");
    assert!(prompt.contains("Keep output concise"), "Default prompt should clamp output shape");
    assert!(
        prompt.contains("Verify changes yourself"),
        "Default prompt should require verification before finalizing"
    );
    assert!(
        prompt.contains("Keep user updates brief and high-signal"),
        "Default prompt should constrain progress updates"
    );
}

#[test]
fn test_default_prompt_omits_removed_model_facing_tool_names() {
    let prompt = default_system_prompt();

    assert_no_removed_model_facing_tool_names(prompt);
    assert!(prompt.contains("exec_command"), "Default prompt should keep baseline shell guidance");
}

#[tokio::test]
async fn test_composed_default_prompt_omits_removed_model_facing_tool_names() {
    let mut config = VTCodeConfig::default();
    config.agent.system_prompt_mode = SystemPromptMode::Default;
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;
    config.agent.instruction_max_bytes = 0;

    let prompt = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert_no_removed_model_facing_tool_names(&prompt);
    assert!(prompt.contains("exec_command"), "Composed default prompt should keep baseline shell guidance");
    assert!(prompt.contains("## Shell Profile"));
    assert!(prompt.contains("controls prompt examples and expected command syntax only"));
}

#[tokio::test]
async fn test_composed_prompts_render_explicit_shell_profiles() {
    let project_root = PathBuf::from(".");
    let mut config = VTCodeConfig::default();
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;
    config.agent.instruction_max_bytes = 0;

    config.agent.shell_prompt_profile = ShellPromptProfile::UnixLike;
    let unix_prompt = compose_system_instruction_text(&project_root, Some(&config), None).await;
    assert!(unix_prompt.contains("Active shell profile: `unix_like`"));
    assert!(unix_prompt.contains("does not rewrite GNU flags for macOS BSD tools"));
    assert!(unix_prompt.contains("does not translate GNU-to-BSD"));

    config.agent.shell_prompt_profile = ShellPromptProfile::PowerShell;
    let powershell_prompt = compose_system_instruction_text(&project_root, Some(&config), None).await;
    assert!(powershell_prompt.contains("Active shell profile: `powershell`"));
    assert!(powershell_prompt.contains("`Get-ChildItem`"));
    assert!(powershell_prompt.contains("use WSL"));
    assert!(powershell_prompt.contains("Unix-to-PowerShell"));
    assert!(!powershell_prompt.contains("`ls`, `rg`, `find`, `cat`, `sed`, and `awk`"));
}

#[test]
fn test_planning_notice_omits_removed_model_facing_tool_names() {
    assert_no_removed_model_facing_tool_names(PLANNING_WORKFLOW_READ_ONLY_NOTICE_LINE);
    assert!(PLANNING_WORKFLOW_READ_ONLY_NOTICE_LINE.contains("exec_command"));
    assert!(PLANNING_WORKFLOW_READ_ONLY_NOTICE_LINE.contains("apply_patch"));
}

#[test]
fn test_all_prompt_modes_treat_completion_as_checkpoint_not_proof() {
    for (mode_name, prompt) in [
        ("default", default_system_prompt()),
        ("minimal", minimal_system_prompt()),
        ("lightweight", default_lightweight_prompt()),
        ("specialized", specialized_instruction_text().as_str()),
    ] {
        assert!(
            prompt.contains("completion language as a checkpoint")
                || prompt.contains("Verify changes yourself")
                || prompt.contains("verification"),
            "{mode_name} prompt should include verification guidance"
        );
    }
}

#[test]
fn test_prompts_encode_explicit_delegation_contract() {
    let prompt = default_system_prompt();

    assert!(
        prompt.contains("Keep control on the main thread"),
        "Default prompt should keep control on the main thread"
    );
    assert!(
        prompt.contains("Delegate bounded, independent work"),
        "Default prompt should restrict delegation to bounded independent work"
    );
    assert!(
        minimal_system_prompt().contains("bound delegation/skills"),
        "Minimal prompt should preserve the delegation contract"
    );
}

#[test]
fn test_default_prompt_includes_grounding_and_action_bias() {
    let prompt = default_system_prompt();
    assert!(
        prompt.contains("Never speculate about code you have not opened"),
        "Default prompt should include grounding guidance"
    );
    assert!(
        prompt.contains("Make only requested changes"),
        "Default prompt should include anti-overengineering guidance"
    );
    assert!(
        prompt.contains("use tools to implement directly"),
        "Default prompt should include action bias for tool-using agents"
    );
}

#[test]
fn test_default_prompt_omits_accuracy_addendum() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let config = VTCodeConfig::default();
    let prompt = runtime.block_on(compose_system_instruction_text(&PathBuf::from("."), Some(&config), None));

    assert!(
        !prompt.contains("## Accuracy Optimization"),
        "Runtime prompt should omit the accuracy optimization section"
    );
    assert!(prompt.contains("do not guess"), "Prompt should still preserve the uncertainty guardrail");
}

#[tokio::test]
async fn test_generated_prompts_keep_operating_profiles_bounded() {
    let project_root = PathBuf::from(".");

    for (mode_name, mode) in [
        ("default", SystemPromptMode::Default),
        ("minimal", SystemPromptMode::Minimal),
        ("lightweight", SystemPromptMode::Lightweight),
        ("specialized", SystemPromptMode::Specialized),
    ] {
        let mut config = VTCodeConfig::default();
        config.agent.system_prompt_mode = mode;
        config.agent.include_temporal_context = false;
        config.agent.include_working_directory = false;
        config.agent.instruction_max_bytes = 0;

        let result = compose_system_instruction_text(&project_root, Some(&config), None).await;

        assert!(result.contains("## Contract"), "{mode_name} prompt should reuse the canonical base prompt");
        assert!(
            result.matches("## Operating Profile").count() == 1,
            "{mode_name} prompt should add only one operating profile"
        );
    }
}

#[test]
fn test_search_guidance_prefers_structural_and_rg() {
    let guidelines = generate_tool_guidelines_for_profile(
        &[tools::EXEC_COMMAND.to_string()],
        None,
        ResolvedShellPromptProfile::UnixLike,
    );
    assert!(guidelines.contains("Browse: `exec_command.cmd`"));
    for command in ["ls", "rg", "find", "cat", "sed", "awk"] {
        assert!(guidelines.contains(&format!("`{command}`")), "Unix browse guidance should include {command}");
    }
    assert!(guidelines.contains("git diff -- <path>"), "Tool guidance should keep diff guidance explicit");
}

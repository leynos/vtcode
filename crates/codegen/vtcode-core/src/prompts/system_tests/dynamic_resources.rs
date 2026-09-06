//! Dynamic tools, language hints, and workspace-resource regressions.

use super::*;

// ENHANCEMENT TESTS

#[tokio::test]
async fn test_dynamic_guidelines_read_only() {
    use crate::config::types::CapabilityLevel;

    let mut config = VTCodeConfig::default();
    config.agent.system_prompt_mode = SystemPromptMode::Default;

    let ctx = PromptContext {
        capability_level: Some(CapabilityLevel::FileReading),
        ..PromptContext::default()
    };

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), Some(&ctx)).await;

    assert!(
        result.contains("Capabilities: read-only"),
        "Should detect read-only capabilities when no edit/write/exec tools available"
    );
    assert!(result.contains("do not modify files"), "Should explain read-only constraints");
}

#[tokio::test]
async fn test_dynamic_guidelines_tool_preferences() {
    let config = VTCodeConfig::default();

    let mut ctx = PromptContext::default();
    ctx.add_tool(tools::EXEC_COMMAND.to_string());
    ctx.add_tool(tools::WRITE_STDIN.to_string());
    ctx.add_tool(tools::APPLY_PATCH.to_string());

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), Some(&ctx)).await;

    assert!(
        result.contains("exec_command") && result.contains("apply_patch"),
        "Should suggest baseline shell and patch tools"
    );
    assert_no_removed_model_facing_tool_names(&result);
}

#[tokio::test]
async fn test_live_prompt_renders_workspace_language_hints() {
    let workspace = tempfile::TempDir::new().expect("workspace tempdir");
    std::fs::create_dir_all(workspace.path().join("src")).expect("create src");
    std::fs::create_dir_all(workspace.path().join("web")).expect("create web");
    std::fs::write(workspace.path().join("src/lib.rs"), "fn alpha() {}\n").expect("write rust");
    std::fs::write(workspace.path().join("web/app.ts"), "const app = 1;\n").expect("write ts");

    let config = VTCodeConfig::default();
    let ctx = PromptContext::from_workspace_tools(workspace.path(), [tools::EXEC_COMMAND]);
    let result = compose_system_instruction_text(workspace.path(), Some(&config), Some(&ctx)).await;

    assert!(result.contains("## Environment"));
    assert!(result.contains("Rust, TypeScript"));
    assert!(result.contains("structural-search `lang`"));
}

#[tokio::test]
async fn test_live_prompt_omits_workspace_language_hints_without_languages() {
    let workspace = tempfile::TempDir::new().expect("workspace tempdir");
    let config = VTCodeConfig::default();
    let ctx = PromptContext::from_workspace_tools(workspace.path(), [tools::EXEC_COMMAND]);
    let result = compose_system_instruction_text(workspace.path(), Some(&config), Some(&ctx)).await;

    assert!(!result.contains("Languages:"));
}

#[tokio::test]
async fn test_live_prompt_omits_project_docs_and_user_instructions_from_base_prompt() {
    let workspace = tempfile::TempDir::new().expect("workspace tempdir");
    std::fs::write(workspace.path().join("AGENTS.md"), "- Root summary\n\nFollow the root guidance.\n")
        .expect("write agents");

    let mut config = VTCodeConfig::default();
    config.agent.user_instructions = Some("keep responses terse".to_string());
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = false;
    config.agent.instruction_max_bytes = 4096;

    let result = compose_system_instruction_text(workspace.path(), Some(&config), None).await;

    assert!(!result.contains("## AGENTS.MD INSTRUCTION HIERARCHY"));
    assert!(!result.contains("### Instruction map"));
    assert!(!result.contains("### Key points"));
    assert!(!result.contains("keep responses terse"));
    assert!(!result.contains("Root summary"));
    assert!(!result.contains("Follow the root guidance."));
}

#[tokio::test]
async fn test_workspace_prompt_resources_override_base_and_keep_dynamic_sections() {
    use crate::skills::model::{SkillMetadata, SkillScope};

    let workspace = tempfile::TempDir::new().expect("workspace tempdir");
    let prompts_dir = workspace.path().join(".vtcode/prompts");
    std::fs::create_dir_all(&prompts_dir).expect("create prompts dir");
    std::fs::write(prompts_dir.join("system.md"), "# Workspace system base").expect("system");
    std::fs::write(prompts_dir.join("append-system.md"), "Workspace prompt appendix").expect("append");

    let mut config = VTCodeConfig::default();
    config.agent.include_temporal_context = false;
    config.agent.include_working_directory = true;

    let mut ctx = PromptContext::default();
    ctx.add_tool(tools::EXEC_COMMAND.to_string());
    ctx.add_skill_metadata(SkillMetadata {
        name: "skill-creator".to_string(),
        description: "Create skills".to_string(),
        short_description: None,
        path: PathBuf::from("/tmp/skill-creator/SKILL.md"),
        scope: SkillScope::System,
        manifest: None,
    });
    ctx.set_current_directory(workspace.path().to_path_buf());

    let result = compose_system_instruction_text(workspace.path(), Some(&config), Some(&ctx)).await;

    assert!(result.starts_with("# Workspace system base"));
    assert!(result.contains(crate::prompts::runtime_guidance::RUNTIME_GUIDANCE_SECTION));
    assert_eq!(
        result
            .matches(crate::prompts::runtime_guidance::RUNTIME_GUIDANCE_SECTION)
            .count(),
        1
    );
    assert!(result.contains("Workspace prompt appendix"));
    assert!(result.contains("## Active Tools"));
    assert!(result.contains("## Skills"));
    assert!(result.contains("## Environment"));

    let appendix_pos = result.find("Workspace prompt appendix").expect("append text");
    let tools_pos = result.find("## Active Tools").expect("tools section");
    let skills_pos = result.find("## Skills").expect("skills section");
    let env_pos = result.find("## Environment").expect("environment section");

    assert!(appendix_pos < tools_pos);
    assert!(tools_pos < skills_pos);
    assert!(skills_pos < env_pos);
}

//! Temporal, configuration, environment, and skill-layer regressions.

use super::*;

#[tokio::test]
async fn test_temporal_context_inclusion() {
    let mut config = VTCodeConfig::default();
    config.agent.include_temporal_context = true;
    config.prompt_cache.cache_friendly_prompt_shaping = false;
    config.agent.temporal_context_use_utc = false; // Local time

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(result.contains("Time:"), "Should include temporal context when enabled");
    let env_pos = result.find("## Environment");
    let temporal_pos = result.find("Time:");
    if let (Some(t), Some(e)) = (temporal_pos, env_pos) {
        assert!(t > e, "Temporal context should appear inside the environment section");
    }
}

#[tokio::test]
async fn test_temporal_context_utc_format() {
    let mut config = VTCodeConfig::default();
    config.agent.include_temporal_context = true;
    config.prompt_cache.cache_friendly_prompt_shaping = false;
    config.agent.temporal_context_use_utc = true; // UTC format

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(result.contains("UTC"), "Should indicate UTC when temporal_context_use_utc is true");
    assert!(result.contains("T") && result.contains("Z"), "Should use RFC3339 format for UTC (contains T and Z)");
}

#[tokio::test]
async fn test_temporal_context_disabled() {
    let mut config = VTCodeConfig::default();
    config.agent.include_temporal_context = false;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(!result.contains("Time:"), "Should not include temporal context when disabled");
}

#[tokio::test]
async fn test_cache_friendly_temporal_context_stays_out_of_base_prompt() {
    let mut config = VTCodeConfig::default();
    config.agent.include_temporal_context = true;
    config.prompt_cache.cache_friendly_prompt_shaping = true;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(result.contains("Time:"), "Session-start time should be frozen in the cached prompt");
}

#[tokio::test]
async fn test_configuration_awareness_stays_behaviour_focused() {
    let mut config = VTCodeConfig::default();
    config.security.human_in_the_loop = true;
    config.chat.ask_questions.enabled = false;
    config.mcp.enabled = true;
    config.ide_context.enabled = true;
    config.ide_context.inject_into_prompt = true;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(result.contains("## Environment"));
    assert!(!result.contains("approval may gate"));
    assert!(result.contains("request_user_input"));
    assert!(result.contains("Sources: prefer MCP"));
    assert!(!result.contains("PTY functionality"));
    assert!(!result.contains("Loop guards"));
    assert!(!result.contains(".vtcode/context/tool_outputs/"));
    assert!(!result.contains("IDE context:"));
}

#[tokio::test]
async fn test_configuration_awareness_mentions_reduced_approval_when_disabled() {
    let mut config = VTCodeConfig::default();
    config.security.human_in_the_loop = false;

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(!result.contains("approval reduced by config"));
}

#[tokio::test]
async fn test_default_environment_omits_default_interaction_guidance() {
    let config = VTCodeConfig::default();

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), None).await;

    assert!(!result.contains("Interaction:"), "Default-on interaction guidance should stay out of the prompt");
}

#[tokio::test]
async fn test_working_directory_inclusion() {
    let mut config = VTCodeConfig::default();
    config.agent.include_working_directory = true;

    let mut ctx = PromptContext::default();
    ctx.set_current_directory(PathBuf::from("/tmp/test"));

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), Some(&ctx)).await;

    assert!(result.contains("Working directory"), "Should include working directory label");
    assert!(result.contains("/tmp/test"), "Should show actual directory path");
    let wd_pos = result.find("Working directory");
    let env_pos = result.find("## Environment");
    if let (Some(w), Some(e)) = (wd_pos, env_pos) {
        assert!(w > e, "Working directory should appear inside the environment section");
    }
}

#[tokio::test]
async fn test_working_directory_disabled() {
    let mut config = VTCodeConfig::default();
    config.agent.include_working_directory = false;

    let mut ctx = PromptContext::default();
    ctx.set_current_directory(PathBuf::from("/tmp/test"));

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), Some(&ctx)).await;

    assert!(!result.contains("Working directory"), "Should not include working directory when disabled");
}

#[tokio::test]
async fn test_backward_compatibility() {
    let config = VTCodeConfig::default();

    // Old signature: no prompt context
    let result = compose_system_instruction_text(
        &PathBuf::from("."),
        Some(&config),
        None, // No context - backward compatible
    )
    .await;

    // Should still work without new features
    assert!(result.len() > 600, "Should generate substantial prompt");
    assert!(result.contains("VT Code"), "Should contain base prompt content");
    // Should not have dynamic guidelines without context
    assert!(!result.contains("## Active Tools"), "Should not have tool guidelines without prompt context");
}

#[tokio::test]
async fn test_all_enhancements_combined() {
    use crate::skills::model::{SkillMetadata, SkillScope};

    let mut config = VTCodeConfig::default();
    config.agent.include_temporal_context = true;
    config.agent.include_working_directory = true;
    config.prompt_cache.cache_friendly_prompt_shaping = false;

    let mut ctx = PromptContext::default();
    ctx.add_tool(tools::APPLY_PATCH.to_string());
    ctx.add_tool(tools::EXEC_COMMAND.to_string());
    ctx.infer_capability_level();
    ctx.set_current_directory(PathBuf::from("/workspace"));
    ctx.add_skill_metadata(SkillMetadata {
        name: "rust-skills".to_string(),
        description: "Rust coding guidance".to_string(),
        short_description: None,
        path: PathBuf::from("/tmp/rust-skills/SKILL.md"),
        scope: SkillScope::System,
        manifest: None,
    });

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), Some(&ctx)).await;

    // Verify all enhancements present
    assert!(result.contains("## Active Tools"), "Should have dynamic guidelines");
    assert!(result.contains("## Skills"), "Should have lean skills routing");
    assert!(result.contains("## Environment"), "Should have environment addenda");
    assert!(result.contains("Time:"), "Should have temporal context");
    assert!(result.contains("Working directory"), "Should have working directory");
    assert!(result.contains("/workspace"), "Should show workspace path");

    // Verify specific guideline for this tool set
    assert!(result.contains("after inspection"), "Should have read-before-edit guideline");
    assert_no_removed_model_facing_tool_names(&result);
}

#[tokio::test]
async fn test_prompt_layers_render_in_stable_order() {
    use crate::skills::model::{SkillMetadata, SkillScope};

    let mut config = VTCodeConfig::default();
    config.agent.include_temporal_context = true;
    config.agent.include_working_directory = true;

    let mut ctx = PromptContext::default();
    ctx.add_tool(tools::EXEC_COMMAND.to_string());
    ctx.add_tool(tools::APPLY_PATCH.to_string());
    ctx.add_skill_metadata(SkillMetadata {
        name: "skill-creator".to_string(),
        description: "Create skills".to_string(),
        short_description: None,
        path: PathBuf::from("/tmp/skill-creator/SKILL.md"),
        scope: SkillScope::System,
        manifest: None,
    });
    ctx.add_language("Rust".to_string());
    ctx.set_current_directory(PathBuf::from("/workspace"));

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), Some(&ctx)).await;

    let mode_pos = result.find("## Operating Profile").expect("operating profile section");
    let tools_pos = result.find("## Active Tools").expect("tools section");
    let skills_pos = result.find("## Skills").expect("skills section");
    let env_pos = result.find("## Environment").expect("environment section");

    assert!(mode_pos < tools_pos, "operating profile should precede tools");
    assert!(tools_pos < skills_pos, "tools should precede skills");
    assert!(skills_pos < env_pos, "skills should precede environment");
}

#[tokio::test]
async fn test_skills_section_stays_lean_and_routing_focused() {
    use crate::skills::model::SkillScope;
    use crate::skills::types::SkillManifest;

    let config = VTCodeConfig::default();
    let mut ctx = PromptContext::default();
    ctx.available_skill_metadata.push(crate::skills::model::SkillMetadata {
        name: "skill-creator".to_string(),
        description: "Create or update skills".to_string(),
        short_description: None,
        path: PathBuf::from("/tmp/skill-creator/SKILL.md"),
        scope: SkillScope::System,
        manifest: Some(
            SkillManifest {
                when_to_use: Some("Use when creating or updating a skill.".to_string()),
                when_not_to_use: Some("Avoid for unrelated implementation work.".to_string()),
                ..SkillManifest::default()
            }
            .into(),
        ),
    });

    let result = compose_system_instruction_text(&PathBuf::from("."), Some(&config), Some(&ctx)).await;

    assert!(result.contains("## Skills"));
    assert!(result.contains("skill-creator: Create or update skills"));
    assert!(result.contains("Use a skill only when the user names it"));
    assert!(!result.contains("Discovery: Available skills are listed"));
    assert!(!result.contains("/tmp/skill-creator/SKILL.md"));
    assert!(!result.contains("use: Use when creating or updating a skill."));
    assert!(!result.contains("avoid: Avoid for unrelated implementation work."));
}

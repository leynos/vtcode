//! Tests ACP tool catalog exposure for built-in and advanced profiles.
use super::support::{build_agent, build_agent_with_tools_config, definition_names};
use crate::zed::types::ToolRuntime;
use assert_fs::TempDir;
use vtcode_core::config::constants::tools;
use vtcode_core::config::{ToolProfile, ToolsConfig};

#[tokio::test]
async fn primary_agent_catalogues_respect_inspection_and_mutation_permissions() {
    let temp = TempDir::new().expect("create isolated ACP workspace");
    let agent = build_agent(temp.path()).await;
    let enabled_tools: Vec<_> = agent
        .tool_availability(true, false)
        .into_iter()
        .filter_map(|(tool, runtime)| match runtime {
            ToolRuntime::Enabled => Some(tool),
            ToolRuntime::Disabled => None,
        })
        .collect();

    let duck_names = definition_names(
        agent
            .tool_definitions(true, &enabled_tools, "duck")
            .expect("resolve duck catalogue"),
    );
    let plan_names = definition_names(
        agent
            .tool_definitions(true, &enabled_tools, "plan")
            .expect("resolve planning catalogue"),
    );
    let build_names = definition_names(
        agent
            .tool_definitions(true, &enabled_tools, "build")
            .expect("resolve build catalogue"),
    );

    assert_eq!(duck_names, vec![tools::LIST_FILES.to_string(), tools::TASK_TRACKER.to_string()]);
    // Planning permits shell inspection; its execution policy separately
    // rejects mutations. Duck deliberately has no shell capability.
    assert_eq!(
        plan_names,
        vec![
            tools::LIST_FILES.to_string(),
            tools::EXEC_COMMAND.to_string(),
            tools::TASK_TRACKER.to_string(),
        ]
    );
    let removed_tool = format!("switch_{}", "mode");
    assert!(!build_names.contains(&removed_tool));
    assert!(build_names.contains(&tools::LIST_FILES.to_string()));
    assert!(build_names.contains(&tools::EXEC_COMMAND.to_string()));
    assert!(build_names.contains(&tools::WRITE_STDIN.to_string()));
    assert!(build_names.contains(&tools::APPLY_PATCH.to_string()));
    assert!(!build_names.contains(&tools::CODE_SEARCH.to_string()));
}

#[tokio::test]
async fn advanced_global_profile_expands_acp_local_catalogue() {
    let temp = TempDir::new().unwrap();
    let tools_config = ToolsConfig {
        profile: ToolProfile::AdvancedVtCode,
        ..ToolsConfig::default()
    };
    let agent = build_agent_with_tools_config(temp.path(), tools_config).await;
    let local_names = definition_names(agent.acp_tool_registry.definitions_for(&[], true));

    assert!(local_names.contains(&tools::EXEC_COMMAND.to_string()));
    assert!(local_names.contains(&tools::WRITE_STDIN.to_string()));
    assert!(local_names.contains(&tools::APPLY_PATCH.to_string()));
    assert!(local_names.contains(&tools::CODE_SEARCH.to_string()));
}

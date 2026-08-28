//! Tests attachment and availability of optional ACP subagent support.
use super::super::optional_setup::{AcpSubagentControllerSetup, OptionalSetupOutcome, attach_acp_subagent_controller};
use super::support::{build_agent, build_agent_with_config, core_agent_config, definition_names};
use assert_fs::TempDir;
use serde_json::json;
use vtcode_config::WorkspaceLifecycleHooks;
use vtcode_config::core::CustomProviderConfig;
use vtcode_core::config::constants::tools;
use vtcode_core::config::{ToolsConfig, VTCodeConfig};
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;

#[tokio::test]
async fn enabled_subagents_are_exposed_to_acp_build_agents() {
    let temp = TempDir::new().unwrap();
    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.subagents.enabled = true;
    vt_cfg.subagents.background.auto_restore = false;
    let agent = build_agent_with_config(temp.path(), ToolsConfig::default(), Some(&vt_cfg)).await;

    assert!(agent.local_tool_registry.has_subagent_controller());
    let names = definition_names(agent.tool_definitions(true, &[], "build").expect("build agent tools"));
    assert!(names.iter().any(|name| name == tools::AGENT));
}

#[tokio::test]
async fn launch_controller_is_gated_for_primary_agent_hooks_without_global_workspace_hooks() {
    let temp = TempDir::new().unwrap();
    let agent_dir = temp.path().join(".vtcode/agents");
    std::fs::create_dir_all(&agent_dir).expect("create project agent directory");
    std::fs::write(
        agent_dir.join("duck.md"),
        r#"---
name: duck
description: Project duck agent
permissions:
  default: ask
hooks:
  PreToolUse:
    - matcher: Bash
      hooks:
        - command: echo primary-agent-hook
mode: primary
---
Project primary prompt.
"#,
    )
    .expect("write project primary agent");

    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.subagents.enabled = true;
    vt_cfg.subagents.background.auto_restore = false;
    assert!(
        vt_cfg
            .workspace_lifecycle_hooks
            .as_ref()
            .is_none_or(WorkspaceLifecycleHooks::is_empty)
    );

    let agent = build_agent_with_config(temp.path(), ToolsConfig::default(), Some(&vt_cfg)).await;
    let launch_runtime = &agent.launch_workspace_runtime;

    assert!(launch_runtime.workspace_hooks_gated);
    assert!(launch_runtime.local_tool_registry.has_subagent_controller());
}

#[tokio::test]
async fn optional_setup_reports_subagents_disabled_by_provider_concurrency_policy() {
    let temp = TempDir::new().unwrap();
    let registry = CoreToolRegistry::new(temp.path().to_path_buf()).await;
    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.subagents.enabled = true;
    let core_config = core_agent_config(temp.path());
    let provider: CustomProviderConfig = serde_json::from_value(json!({
        "name": "test-provider",
        "display_name": "Test provider",
        "base_url": "https://example.invalid/v1",
        "model": "test-model",
        "request_policy": { "max_in_flight_requests": 1 }
    }))
    .expect("valid custom provider configuration");

    let outcome = attach_acp_subagent_controller(AcpSubagentControllerSetup {
        owner_session_id: None,
        registry: &registry,
        agent_config: &core_config,
        custom_providers: &[provider],
        vt_config: Some(&vt_cfg),
        workspace_gated: false,
    })
    .await;

    assert_eq!(outcome, OptionalSetupOutcome::DisabledByPolicy);
    assert!(!registry.has_subagent_controller());
}

#[tokio::test]
async fn optional_setup_reports_subagent_initialization_failure() {
    let temp = TempDir::new().unwrap();
    let registry = CoreToolRegistry::new(temp.path().to_path_buf()).await;
    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.subagents.enabled = true;
    vt_cfg.subagents.background.auto_restore = false;
    vt_cfg.hooks.lifecycle.session_start = serde_json::from_value(json!([
        { "matcher": "[", "hooks": [] }
    ]))
    .expect("lifecycle hook configuration shape");
    let core_config = core_agent_config(temp.path());

    let outcome = attach_acp_subagent_controller(AcpSubagentControllerSetup {
        owner_session_id: None,
        registry: &registry,
        agent_config: &core_config,
        custom_providers: &[],
        vt_config: Some(&vt_cfg),
        workspace_gated: false,
    })
    .await;

    assert!(matches!(outcome, OptionalSetupOutcome::Failed(_)));
    assert!(!registry.has_subagent_controller());
}
#[tokio::test]
async fn unavailable_subagent_controller_keeps_agent_tool_hidden() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;

    assert!(!agent.local_tool_registry.has_subagent_controller());
    let names = definition_names(agent.tool_definitions(true, &[], "build").expect("build agent tools"));
    assert!(!names.iter().any(|name| name == tools::AGENT));
}

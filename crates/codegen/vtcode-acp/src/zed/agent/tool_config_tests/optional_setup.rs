//! Tests shared setup outcomes when ACP attaches optional components together.
use super::super::optional_setup::{
    AcpSubagentControllerSetup, OptionalSetupOutcome, attach_acp_mcp_client, attach_acp_subagent_controller,
};
use super::support::core_agent_config;
use assert_fs::TempDir;
use vtcode_core::config::VTCodeConfig;
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;

#[tokio::test]
async fn optional_setup_reports_configuration_disabled_components() {
    let temp = TempDir::new().unwrap();
    let registry = CoreToolRegistry::new(temp.path().to_path_buf()).await;
    let core_config = core_agent_config(temp.path());
    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.subagents.enabled = false;
    vt_cfg.mcp.enabled = false;

    let subagent_outcome = attach_acp_subagent_controller(AcpSubagentControllerSetup {
        owner_session_id: None,
        registry: &registry,
        agent_config: &core_config,
        custom_providers: &[],
        vt_config: Some(&vt_cfg),
        workspace_gated: false,
    })
    .await;
    let mcp_outcome = attach_acp_mcp_client(&registry, Some(&vt_cfg), temp.path()).await;

    assert_eq!(subagent_outcome, OptionalSetupOutcome::DisabledByConfiguration);
    assert_eq!(mcp_outcome, OptionalSetupOutcome::DisabledByConfiguration);
    assert!(!registry.has_subagent_controller());
    assert!(registry.mcp_client().is_none());
}
#[tokio::test]
async fn optional_setup_reports_successfully_attached_components() {
    let temp = TempDir::new().unwrap();
    let registry = CoreToolRegistry::new(temp.path().to_path_buf()).await;
    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.subagents.enabled = true;
    vt_cfg.subagents.background.auto_restore = false;
    vt_cfg.mcp.enabled = true;
    let core_config = core_agent_config(temp.path());

    let subagent_outcome = attach_acp_subagent_controller(AcpSubagentControllerSetup {
        owner_session_id: None,
        registry: &registry,
        agent_config: &core_config,
        custom_providers: &[],
        vt_config: Some(&vt_cfg),
        workspace_gated: false,
    })
    .await;
    let mcp_outcome = attach_acp_mcp_client(&registry, Some(&vt_cfg), temp.path()).await;

    assert_eq!(subagent_outcome, OptionalSetupOutcome::Attached);
    assert!(matches!(
        mcp_outcome,
        OptionalSetupOutcome::Attached | OptionalSetupOutcome::AttachedWithDegradation(_)
    ));
    assert!(registry.has_subagent_controller());
    assert!(registry.mcp_client().is_some());
}

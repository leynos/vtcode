//! Tests optional ACP MCP client attachment and provider startup degradation.
use super::super::{OptionalSetupOutcome, attach_acp_mcp_client};
use super::support::core_agent_config;
use assert_fs::TempDir;
use serde_json::json;
use vtcode_core::config::VTCodeConfig;
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;

#[tokio::test]
async fn optional_mcp_setup_reports_provider_startup_failure_and_keeps_client_attached() {
    let temp = TempDir::new().unwrap();
    let registry = CoreToolRegistry::new(temp.path().to_path_buf()).await;
    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.mcp.enabled = true;
    vt_cfg.mcp.providers = vec![
        serde_json::from_value(json!({
            "name": "unavailable-provider",
            "command": "/path/that/does/not/exist/vtcode-mcp-test",
            "args": [],
            "startup_timeout_ms": 100
        }))
        .expect("valid stdio provider configuration"),
    ];

    let outcome = attach_acp_mcp_client(&registry, Some(&vt_cfg), temp.path()).await;

    assert!(matches!(outcome, OptionalSetupOutcome::AttachedWithDegradation(_)));
    assert!(registry.mcp_client().is_some());
}

//! Tests shared tool-loop limits and request pacing configuration.
use super::support::build_agent_with_tools_config;
use assert_fs::TempDir;
use std::time::Duration;
use vtcode_config::constants::tool_limits;
use vtcode_core::config::{ToolsConfig, tool_call_delay_for_rate};

#[test]
fn default_zed_tool_config_uses_shared_tool_loop_budget() {
    assert_eq!(ToolsConfig::default().max_tool_loops, tool_limits::DEFAULT_MAX_TOOL_LOOPS);
}

#[test]
fn tool_call_delay_for_rate_ignores_unset_or_zero_limits() {
    assert_eq!(tool_call_delay_for_rate(None), None);
    assert_eq!(tool_call_delay_for_rate(Some(0)), None);
}

#[test]
fn tool_call_delay_for_rate_uses_per_second_interval() {
    assert_eq!(tool_call_delay_for_rate(Some(4)), Some(Duration::from_millis(250)));
}

#[tokio::test]
async fn tool_loop_limit_uses_tools_config() {
    let temp = TempDir::new().unwrap();
    let tools_config = ToolsConfig { max_tool_loops: 2, ..ToolsConfig::default() };
    let agent = build_agent_with_tools_config(temp.path(), tools_config).await;

    assert!(!agent.tool_loop_limit_reached(0));
    assert!(!agent.tool_loop_limit_reached(1));
    assert!(agent.tool_loop_limit_reached(2));
    assert!(agent.tool_loop_limit_message().contains("maximum tool loops (2)"));
}

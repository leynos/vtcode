//! Tests for compiled defaults and tool profile configuration.

use super::*;

#[test]
fn default_config_selects_build_primary_agent() {
    assert_eq!(VTCodeConfig::default().default_primary_agent, "build");
}

#[test]
fn compiled_default_config_contains_release_loop_budgets() {
    let config = VTCodeConfig::default();

    assert_eq!(config.agent.harness.max_tool_calls_per_turn, tool_limits::DEFAULT_MAX_TOOL_CALLS_PER_TURN);
    assert_eq!(config.tools.max_tool_loops, tool_limits::DEFAULT_MAX_TOOL_LOOPS);
    assert_eq!(config.automation.full_auto.max_turns, tool_limits::DEFAULT_FULL_AUTO_MAX_TURNS);
    assert_eq!(config.agent.max_conversation_turns, tool_limits::DEFAULT_MAX_CONVERSATION_TURNS);
}

#[test]
fn tool_profile_parses_and_serialises_in_tools_table() {
    let config: VTCodeConfig = toml::from_str(
        r#"
[tools]
profile = "advanced_vtcode"
"#,
    )
    .expect("tool profile should parse from the tools table");
    assert_eq!(config.tools.profile, crate::core::ToolProfile::AdvancedVtCode);

    let serialised = toml::to_string(&config).expect("configuration should serialise");
    assert!(serialised.contains("[tools]"));
    assert!(serialised.contains("profile = \"advanced_vtcode\""));
}

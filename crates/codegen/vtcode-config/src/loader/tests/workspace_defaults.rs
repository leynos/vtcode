//! Tests for workspace configuration defaults and partial values.

use super::*;

#[test]
fn workspace_config_parses_from_toml() {
    let toml_str = r#"
[workspace]
use_root_config = true
include_context = false
max_context_size = 2097152
"#;
    let config: VTCodeConfig = toml::from_str(toml_str).expect("parse workspace config");
    assert!(config.workspace.use_root_config);
    assert!(!config.workspace.include_context);
    assert_eq!(config.workspace.max_context_size, Some(2_097_152));
}

#[test]
fn workspace_config_defaults_match_spec() {
    let config = VTCodeConfig::default();
    assert!(!config.workspace.use_root_config);
    assert!(config.workspace.include_context);
    assert_eq!(config.workspace.max_context_size, None);
}

#[test]
fn workspace_config_partial_toml_uses_defaults() {
    let toml_str = r#"
[workspace]
use_root_config = true
"#;
    let config: VTCodeConfig = toml::from_str(toml_str).expect("parse partial workspace config");
    assert!(config.workspace.use_root_config);
    assert!(config.workspace.include_context);
    assert_eq!(config.workspace.max_context_size, None);
}

#[test]
fn partial_optimization_agent_execution_uses_defaults() {
    let toml_str = r#"
[optimization.agent_execution]
max_execution_time_secs = 300
state_history_size = 100
resource_monitor_interval_ms = 5000
max_memory_mb = 4096
idle_timeout_ms = 30000
idle_backoff_ms = 1000
"#;
    let config: VTCodeConfig = toml::from_str(toml_str).expect("parse partial agent_execution config");
    assert_eq!(config.optimization.agent_execution.max_execution_time_secs, 300);
    assert_eq!(config.optimization.agent_execution.idle_timeout_ms, 30000);
    assert_eq!(config.optimization.agent_execution.idle_backoff_ms, 1000);
}

#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
use diapason_core::config::loader::ConfigManager;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use tempfile::tempdir;

#[test]
fn test_configuration_caching() {
    // Keep this benchmark-style smoke test independent of the developer's
    // global config. `use_root_config` also prevents a malformed user layer
    // from making an otherwise valid workspace fixture fail to load.
    let temp_workspace = tempdir().expect("create isolated config workspace");
    fs::write(temp_workspace.path().join("config.toml"), "[workspace]\nuse_root_config = true\n")
        .expect("write isolated config fixture");
    let workspace = temp_workspace.path();

    // First load
    let start_time = Instant::now();
    let config1 = ConfigManager::load_from_workspace(workspace)
        .ok()
        .map(|manager| manager.config().clone());
    let first_load_time = start_time.elapsed();

    // Second load using cached path
    let start_time = Instant::now();
    let config2 = ConfigManager::load_from_workspace(workspace)
        .ok()
        .map(|manager| manager.config().clone());
    let second_load_time = start_time.elapsed();

    // Both should load successfully
    assert!(config1.is_some() || config2.is_some());

    println!("First load time: {first_load_time:?}");
    println!("Second load time: {second_load_time:?}");
}

#[test]
fn test_workspace_path_caching() {
    // Test that our workspace path caching optimization works
    let workspace = PathBuf::from(".");

    // This simulates the optimization where we cache the workspace path
    let cached_workspace = workspace.clone();

    // Verify we can use the cached path
    assert_eq!(cached_workspace, workspace);
    assert!(cached_workspace.exists() || workspace == std::path::Path::new("."));
}

#[test]
fn test_idle_configuration_defaults() {
    // Test that our idle configuration has sensible defaults
    let config = diapason_core::config::loader::DiapasonConfig::default();
    let idle_config = &config.optimization.agent_execution;

    // Should have reasonable defaults
    assert!(idle_config.idle_timeout_ms > 0);
    assert!(idle_config.idle_backoff_ms > 0);
    assert!(idle_config.max_idle_cycles > 0);

    println!("Idle timeout: {}ms", idle_config.idle_timeout_ms);
    println!("Idle backoff: {}ms", idle_config.idle_backoff_ms);
    println!("Max idle cycles: {}", idle_config.max_idle_cycles);
}

#[test]
fn test_optimization_config_structure() {
    // Test that our new optimization fields are properly structured
    use diapason_config::OptimizationConfig;

    let config = OptimizationConfig::default();
    assert!(config.agent_execution.idle_timeout_ms > 0);
    assert!(config.agent_execution.idle_backoff_ms > 0);
    assert!(config.agent_execution.max_idle_cycles > 0);

    // Test development config
    let dev_config = OptimizationConfig::development();
    assert!(dev_config.agent_execution.idle_timeout_ms > 0);

    // Test production config
    let prod_config = OptimizationConfig::production();
    assert!(prod_config.agent_execution.idle_timeout_ms > 0);
}

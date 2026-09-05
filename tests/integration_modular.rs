#![allow(
    missing_docs,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
//! Integration tests for modular architecture
//!
//! These tests validate that all refactored modules work together correctly
//! and maintain backward compatibility.

use std::sync::Arc;
use vtcode_commons::StaticWorkspacePaths;
use vtcode_config::defaults::provider::with_config_defaults_provider_for_test;
use vtcode_core::{
    code::code_completion::{CompletionContext, CompletionEngine},
    code::code_quality::{FormattingOrchestrator, LintingOrchestrator, QualityMetrics},
    config::{ConfigManager, ToolPolicy, VTCodeConfig, WorkspacePathsDefaults},
    gemini::{Client, ClientConfig},
};

#[test]
fn test_gemini_module_integration() {
    // Test that we can create a Gemini client with different configurations
    let client = Client::new("test_key".to_string(), "gemini-3-flash-preview".to_string());
    assert_eq!(client.config().user_agent, "vtcode/1.0.0");

    // Test different client configurations
    let high_throughput_config = ClientConfig::high_throughput();
    assert_eq!(high_throughput_config.pool_max_idle_per_host, 20);

    let low_memory_config = ClientConfig::low_memory();
    assert_eq!(low_memory_config.pool_max_idle_per_host, 3);
}

#[test]
#[serial_test::serial]
fn test_config_module_integration() -> anyhow::Result<()> {
    // Test that we can create and use configurations
    let config = VTCodeConfig::default();
    anyhow::ensure!(
        config.agent.provider == vtcode_core::config::constants::defaults::DEFAULT_PROVIDER,
        "default configuration should use the default provider"
    );
    anyhow::ensure!(config.tools.default_policy == ToolPolicy::Prompt, "default tool policy should prompt");

    // An empty workspace still inherits user configuration unless the defaults
    // provider excludes those paths. Keep every candidate inside this fixture.
    let temp_workspace = tempfile::tempdir()?;
    let paths = StaticWorkspacePaths::new(temp_workspace.path(), temp_workspace.path().join(".vtcode"));
    let defaults = WorkspacePathsDefaults::new(Arc::new(paths))
        .with_home_paths(Vec::new())
        .with_system_config_paths(Vec::new());
    let manager = with_config_defaults_provider_for_test(Arc::new(defaults), || {
        ConfigManager::load_from_workspace(temp_workspace.path())
    })?;
    let loaded_config = manager.config();
    anyhow::ensure!(
        loaded_config.agent.provider == vtcode_core::config::constants::defaults::DEFAULT_PROVIDER,
        "isolated configuration should use the default provider"
    );
    anyhow::ensure!(loaded_config.tools.default_policy == ToolPolicy::Prompt, "loaded tool policy should prompt");
    Ok(())
}

#[test]
fn test_code_completion_integration() {
    // Test that we can create completion engine and context
    let _engine = CompletionEngine::new();

    let context = CompletionContext::new(10, 5, "fn test".to_string(), "rust".to_string());

    assert!(context.is_completion_suitable());
    assert_eq!(context.language, "rust");
}

#[test]
fn test_code_quality_integration() {
    // Test that we can create orchestrators
    let _formatting = FormattingOrchestrator::new();
    let _linting = LintingOrchestrator::new();

    // Test quality metrics
    let metrics = QualityMetrics {
        total_files: 10,
        formatted_files: 8,
        lint_errors: 2,
        ..Default::default()
    };

    let score = metrics.quality_score();
    assert!(score > 0.0 && score <= 100.0);
}

#[test]
fn test_backward_compatibility() {
    // Test that all the old import patterns still work
    use vtcode_core::code::code_completion::CompletionEngine;
    use vtcode_core::code::code_quality::FormattingOrchestrator;
    use vtcode_core::config::VTCodeConfig;
    use vtcode_core::gemini::Client;

    // These should all compile and work as before
    let _client = Client::new("key".to_string(), "model".to_string());
    let _config = VTCodeConfig::default();
    let _engine = CompletionEngine::new();
    let _formatter = FormattingOrchestrator::new();
}

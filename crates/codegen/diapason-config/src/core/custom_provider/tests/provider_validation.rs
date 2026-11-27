//! Provider-level validation tests for custom provider configuration.
use std::collections::BTreeMap;
use std::path::PathBuf;

use super::base_provider_config;
use crate::core::custom_provider::{
    CustomProviderCommandAuthConfig, CustomProviderConfig, CustomProviderProfileConfig,
};

#[test]
fn validate_accepts_lowercase_provider_name() {
    let config = base_provider_config();

    assert!(config.validate().is_ok());
    assert_eq!(config.resolved_api_key_env(), "MYCORP_API_KEY");
}

#[test]
fn validate_rejects_invalid_provider_name() {
    let config = CustomProviderConfig {
        name: "My Corp".to_string(),
        display_name: "My Corp".to_string(),
        ..base_provider_config()
    };

    let err = config.validate().expect_err("invalid name should fail");
    assert!(err.contains("must use lowercase letters, digits, hyphens, or underscores"));
}

#[test]
fn validate_rejects_auth_and_api_key_env_together() {
    let config = CustomProviderConfig {
        api_key_env: "MYCORP_API_KEY".to_string(),
        auth: Some(CustomProviderCommandAuthConfig {
            command: "print-token".to_string(),
            ..CustomProviderCommandAuthConfig::default()
        }),
        ..base_provider_config()
    };

    let err = config.validate().expect_err("conflicting auth should fail");
    assert!(err.contains("`auth` cannot be combined with `api_key_env`"));
}

#[test]
fn validate_accepts_command_auth_without_static_env_key() {
    let config = CustomProviderConfig {
        auth: Some(CustomProviderCommandAuthConfig {
            command: "print-token".to_string(),
            args: vec!["--json".to_string()],
            cwd: Some(PathBuf::from("/tmp")),
            ..CustomProviderCommandAuthConfig::default()
        }),
        ..base_provider_config()
    };

    assert!(config.validate().is_ok());
    assert!(config.uses_command_auth());
}

#[test]
fn validate_rejects_empty_model_entry_in_models_list() {
    let config = CustomProviderConfig {
        api_key_env: "MYCORP_API_KEY".to_string(),
        models: vec!["valid-model".to_string(), "   ".to_string()],
        ..base_provider_config()
    };

    let err = config.validate().expect_err("blank models entry should fail");
    assert!(err.contains("`models` entries must not be empty"));
}

#[test]
fn validate_rejects_zero_context_window() {
    let config = CustomProviderConfig { context_window: Some(0), ..base_provider_config() };

    let err = config.validate().expect_err("zero context window should fail");
    assert!(err.contains("`context_window` must be greater than 0"));
}

#[test]
fn validate_rejects_malformed_profile_key() {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        " gpt-5-mini ".to_string(),
        CustomProviderProfileConfig {
            context_window: Some(128_000),
            ..CustomProviderProfileConfig::default()
        },
    );

    let config = CustomProviderConfig { profiles, ..base_provider_config() };

    let err = config.validate().expect_err("profile key with whitespace should fail");
    assert!(err.contains("profile key"));
}

//! Tests for provider construction and the custom-provider registry.
//!
//! The production module and these focused submodules each stay inside the
//! repository's line budget, and the fixtures below hold the provider defaults
//! every case shares.

use super::*;

use diapason_config::core::{CustomProviderApiFormat, CustomProviderConfig, CustomProviderProfileConfig};
use std::collections::BTreeMap;

#[path = "factory_tests/builtin_providers.rs"]
mod builtin_providers;
#[path = "factory_tests/custom_providers.rs"]
mod custom_providers;
#[path = "factory_tests/provider_resolution.rs"]
mod provider_resolution;

/// Build a factory request with every optional field left unset.
fn provider_config(api_key: Option<&str>, model: Option<&str>) -> ProviderConfig {
    ProviderConfig {
        api_key: api_key.map(str::to_owned),
        openai_chatgpt_auth: None,
        copilot_auth: None,
        base_url: None,
        model: model.map(str::to_owned),
        prompt_cache: None,
        timeouts: None,
        openai: None,
        anthropic: None,
        model_behaviour: None,
        workspace_root: None,
    }
}

/// Build a custom provider carrying the defaults these tests share.
///
/// Fields not named here inherit [`CustomProviderConfig::default`], so a schema
/// change, including the pricing default, needs updating in one place rather
/// than in every fixture.
fn custom_provider(
    name: &str,
    api_format: CustomProviderApiFormat,
    model: &str,
    profiles: BTreeMap<String, CustomProviderProfileConfig>,
) -> CustomProviderConfig {
    CustomProviderConfig {
        name: name.to_string(),
        display_name: name.to_string(),
        base_url: "https://llm.example/v1".to_string(),
        api_format,
        model: model.to_string(),
        api_key_env: format!("{}_API_KEY", name.to_ascii_uppercase()),
        profiles,
        ..CustomProviderConfig::default()
    }
}

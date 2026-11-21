//! Tests for custom provider configuration parsing, resolution, and validation.

use super::CustomProviderConfig;

mod profile;
mod provider_validation;
mod request_policy;

fn base_provider_config() -> CustomProviderConfig {
    CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorp".to_string(),
        base_url: "https://llm.example/v1".to_string(),
        model: "gpt-5-mini".to_string(),
        ..CustomProviderConfig::default()
    }
}

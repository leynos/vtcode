//! Tests for LLM provider factory registration and model resolution.
use super::ProviderConfig;
use diapason_config::core::{CustomProviderApiFormat, CustomProviderConfig, CustomProviderProfileConfig};
use std::collections::BTreeMap;

mod builtin;
mod custom;
mod model_resolution;

/// Provider configuration with every optional override cleared.
pub(super) fn base_provider_config() -> ProviderConfig {
    ProviderConfig {
        api_key: None,
        openai_chatgpt_auth: None,
        copilot_auth: None,
        base_url: None,
        model: None,
        prompt_cache: None,
        timeouts: None,
        openai: None,
        anthropic: None,
        model_behaviour: None,
        workspace_root: None,
    }
}

pub(super) fn profile_config(
    api_format: CustomProviderApiFormat,
    context_window: Option<usize>,
    supports_tools: Option<bool>,
) -> CustomProviderProfileConfig {
    CustomProviderProfileConfig {
        api_format,
        context_window,
        supports_tools,
        ..CustomProviderProfileConfig::default()
    }
}

pub(super) fn mixed_profile_provider_config() -> CustomProviderConfig {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "gpt-5-mini".to_string(),
        profile_config(CustomProviderApiFormat::OpenAIChat, Some(256_000), Some(false)),
    );
    profiles.insert(
        "claude-sonnet-5".to_string(),
        profile_config(CustomProviderApiFormat::AnthropicMessages, Some(512_000), Some(true)),
    );

    CustomProviderConfig {
        name: "mixed".to_string(),
        display_name: "Mixed".to_string(),
        base_url: "https://llm.corp.example/v1".to_string(),
        api_key_env: "MIXED_API_KEY".to_string(),
        model: "gpt-5-mini".to_string(),
        models: vec!["gpt-5-mini".to_string(), "claude-sonnet-5".to_string()],
        profiles,
        ..CustomProviderConfig::default()
    }
}

/// Sample Atlas Cloud config used across custom-provider tests.
/// Matches the snippet documented in `docs/providers/atlascloud.md` and
/// `config.toml.example`.
pub(super) fn atlas_cloud_provider_config() -> CustomProviderConfig {
    CustomProviderConfig {
        name: "atlascloud".to_string(),
        display_name: "Atlas Cloud".to_string(),
        base_url: "https://api.atlascloud.ai/v1".to_string(),
        api_key_env: "ATLASCLOUD_API_KEY".to_string(),
        model: "deepseek-ai/deepseek-v4-flash".to_string(),
        models: vec![
            "deepseek-ai/deepseek-v4-flash".to_string(),
            "deepseek-ai/deepseek-v4-pro".to_string(),
            "deepseek-ai/DeepSeek-V3-0324".to_string(),
            "deepseek-ai/DeepSeek-V3.1".to_string(),
            "deepseek-ai/deepseek-r1-0528".to_string(),
            "deepseek-ai/deepseek-ocr".to_string(),
            "qwen/qwen3.6-35b-a3b".to_string(),
            "qwen/qwen3.6-plus".to_string(),
            "qwen/qwen3.5-122b-a10b".to_string(),
            "qwen/qwen3.5-35b-a3b".to_string(),
            "qwen/qwen3-coder-next".to_string(),
            "qwen/qwen3.5-397b-a17b".to_string(),
            "qwen/qwen3-max-2026-01-23".to_string(),
            "qwen/qwen3-235b-a22b-thinking-2507".to_string(),
            "qwen/qwen3-30b-a3b-thinking-2507".to_string(),
            "qwen/qwen3-next-80b-a3b-thinking".to_string(),
            "qwen/qwen3-next-80b-a3b-instruct".to_string(),
            "moonshotai/kimi-k2.7-code".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
            "moonshotai/kimi-k2.5".to_string(),
            "moonshotai/Kimi-K2-Thinking".to_string(),
            "moonshotai/Kimi-K2-Instruct".to_string(),
            "moonshotai/Kimi-K2-Instruct-0905".to_string(),
            "zai-org/glm-5.2".to_string(),
            "zai-org/glm-5.1".to_string(),
            "zai-org/glm-5v-turbo".to_string(),
            "zai-org/glm-5-turbo".to_string(),
            "zai-org/glm-4.7".to_string(),
            "minimaxai/minimax-m3".to_string(),
            "minimaxai/minimax-m2.7".to_string(),
            "minimaxai/minimax-m2.5".to_string(),
            "minimaxai/minimax-m2.1".to_string(),
            "kwaipilot/kat-coder-pro-v2".to_string(),
            "Alibaba-NLP/Tongyi-DeepResearch-30B-A3B".to_string(),
        ],
        ..CustomProviderConfig::default()
    }
}

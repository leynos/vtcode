//! Unit tests for API-key lookup and environment-variable naming.

use super::*;

#[test]
fn gemini_reads_env_var() {
    with_override("GEMINI_API_KEY", Some("test-gemini-key"), || {
        let result = get_api_key("gemini", &default_sources());
        assert_eq!(result.unwrap(), "test-gemini-key");
    });
}

#[test]
fn gemini_falls_back_to_google_api_key() {
    // Clear both GEMINI_API_KEY and set GOOGLE_API_KEY to verify fallback
    with_overrides(
        &[
            ("GEMINI_API_KEY", Some("gemini-primary")),
            ("GOOGLE_API_KEY", Some("google-fallback")),
        ],
        || {
            // With GEMINI_API_KEY set, it should be preferred
            let result = get_api_key("gemini", &default_sources());
            assert_eq!(result.unwrap(), "gemini-primary");
        },
    );
    with_overrides(&[("GEMINI_API_KEY", None), ("GOOGLE_API_KEY", Some("google-fallback"))], || {
        // Without GEMINI_API_KEY, it should fall back to GOOGLE_API_KEY
        let result = get_api_key("gemini", &default_sources());
        assert_eq!(result.unwrap(), "google-fallback");
    });
}

#[test]
fn anthropic_reads_env_var() {
    with_override("ANTHROPIC_API_KEY", Some("test-anthropic-key"), || {
        let result = get_api_key("anthropic", &default_sources());
        assert_eq!(result.unwrap(), "test-anthropic-key");
    });
}

#[test]
fn openai_reads_env_var() {
    with_override("OPENAI_API_KEY", Some("test-openai-key"), || {
        let result = get_api_key("openai", &default_sources());
        assert_eq!(result.unwrap(), "test-openai-key");
    });
}

#[test]
fn deepseek_reads_env_var() {
    with_override("DEEPSEEK_API_KEY", Some("test-deepseek-key"), || {
        let result = get_api_key("deepseek", &default_sources());
        assert_eq!(result.unwrap(), "test-deepseek-key");
    });
}

#[test]
fn qwen_falls_back_to_dashscope() {
    with_overrides(&[("QWEN_API_KEY", None), ("DASHSCOPE_API_KEY", Some("dashscope-key"))], || {
        let result = get_api_key("qwen", &default_sources());
        assert_eq!(result.unwrap(), "dashscope-key");
    });
}

#[test]
fn ollama_allows_empty_key() {
    with_override("OLLAMA_API_KEY", None, || {
        let result = get_api_key("ollama", &default_sources());
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    });
}

#[test]
fn lmstudio_allows_empty_key() {
    with_override("LMSTUDIO_API_KEY", None, || {
        let result = get_api_key("lmstudio", &default_sources());
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    });
}

#[test]
fn ollama_reads_env_var_when_set() {
    with_override("OLLAMA_API_KEY", Some("test-ollama-key"), || {
        let result = get_api_key("ollama", &default_sources());
        assert_eq!(result.unwrap(), "test-ollama-key");
    });
}

#[test]
fn copilot_returns_managed_auth_error() {
    let result = get_api_key("copilot", &default_sources());
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("copilot"));
}

#[test]
fn codex_returns_managed_auth_error() {
    let result = get_api_key("codex", &default_sources());
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("codex"));
}

#[test]
fn unknown_provider_returns_error_with_env_hint() {
    with_override("SOMEUNKNOWN_API_KEY", None, || {
        let result = get_api_key("someunknown", &default_sources());
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("SOMEUNKNOWN_API_KEY"));
    });
}

#[test]
fn poolside_reads_env_var() {
    with_override("POOLSIDE_API_KEY", Some("test-poolside-key"), || {
        let result = get_api_key("poolside", &default_sources());
        assert_eq!(result.unwrap(), "test-poolside-key");
    });
}

#[test]
fn poolside_returns_error_when_missing() {
    with_override("POOLSIDE_API_KEY", None, || {
        let result = get_api_key("poolside", &default_sources());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("POOLSIDE_API_KEY"));
    });
}

#[test]
fn merge_gateway_reads_env_var() {
    with_override("MERGE_GATEWAY_API_KEY", Some("test-merge-gateway-key"), || {
        let result = get_api_key("merge-gateway", &default_sources());
        assert_eq!(result.unwrap(), "test-merge-gateway-key");
    });
}

#[test]
fn meta_reads_provider_specific_env_var() {
    with_overrides(
        &[
            ("META_API_KEY", Some("meta-primary")),
            ("MODEL_API_KEY", Some("model-fallback")),
        ],
        || {
            let result = get_api_key("meta", &default_sources());
            assert_eq!(result.expect("Meta key"), "meta-primary");
        },
    );
}

#[test]
fn meta_falls_back_to_documented_model_api_key() {
    with_overrides(&[("META_API_KEY", None), ("MODEL_API_KEY", Some("model-key"))], || {
        let result = get_api_key("meta", &default_sources());
        assert_eq!(result.expect("Meta fallback key"), "model-key");
    });
}

#[test]
fn meta_missing_key_error_names_both_supported_env_vars() {
    with_overrides(&[("META_API_KEY", None), ("MODEL_API_KEY", None)], || {
        let error = get_api_key("meta", &default_sources()).expect_err("missing Meta key");
        assert!(error.to_string().contains("META_API_KEY or MODEL_API_KEY"));
    });
}

#[test]
fn api_key_env_var_uses_provider_defaults() {
    assert_eq!(api_key_env_var("codex"), "");
    assert_eq!(api_key_env_var("minimax"), "MINIMAX_API_KEY");
    assert_eq!(api_key_env_var("huggingface"), "HF_TOKEN");
    assert_eq!(api_key_env_var("poolside"), "POOLSIDE_API_KEY");
    assert_eq!(api_key_env_var("merge-gateway"), "MERGE_GATEWAY_API_KEY");
    assert_eq!(api_key_env_var("my-corp"), "MY_CORP_API_KEY");
    assert_eq!(api_key_env_var("123corp"), "_123CORP_API_KEY");
}

#[test]
fn resolve_api_key_env_uses_provider_default_for_placeholder() {
    assert_eq!(resolve_api_key_env("minimax", defaults::DEFAULT_API_KEY_ENV), "MINIMAX_API_KEY");
}

#[test]
fn resolve_api_key_env_preserves_explicit_override() {
    assert_eq!(resolve_api_key_env("openai", "CUSTOM_OPENAI_KEY"), "CUSTOM_OPENAI_KEY");
}

#[test]
fn credential_metadata_key_normalizes_provider_and_key() {
    assert_eq!(
        credential_metadata_key(" MyCorp ", "mycorp_billing_key").expect("metadata key"),
        Some("mycorp/MYCORP_BILLING_KEY".to_string())
    );
}

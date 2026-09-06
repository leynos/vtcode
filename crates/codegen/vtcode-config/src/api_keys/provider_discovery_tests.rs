//! Unit tests for provider discovery and credential-detail reporting.

use super::*;

#[test]
fn local_providers_are_always_discovered() {
    // Local providers need no key and should be discoverable with empty env.
    with_overrides(
        &[
            ("OLLAMA_API_KEY", None),
            ("LMSTUDIO_API_KEY", None),
            ("LLAMACPP_API_KEY", None),
        ],
        || {
            assert_eq!(provider_credential_source(Provider::Ollama), Some(CredentialSource::Local));
            assert_eq!(provider_credential_source(Provider::LmStudio), Some(CredentialSource::Local));
            assert_eq!(provider_credential_source(Provider::LlamaCpp), Some(CredentialSource::Local));
        },
    );
}

#[test]
fn copilot_is_managed_auth_discovered() {
    assert_eq!(provider_credential_source(Provider::Copilot), Some(CredentialSource::ManagedAuth));
}

#[test]
fn env_var_makes_provider_discovered() {
    with_override("OPENROUTER_API_KEY", Some("or-test-key"), || {
        assert_eq!(provider_credential_source(Provider::OpenRouter), Some(CredentialSource::Env));
    });
}

#[test]
fn missing_env_var_leaves_provider_undiscovered() {
    with_override("OPENROUTER_API_KEY", None, || {
        assert_eq!(provider_credential_source(Provider::OpenRouter), None);
    });
}

#[test]
fn gemini_alt_env_var_is_discovered() {
    with_overrides(&[("GEMINI_API_KEY", None), ("GOOGLE_API_KEY", Some("g-key"))], || {
        assert_eq!(provider_credential_source(Provider::Gemini), Some(CredentialSource::Env));
    });
}

#[test]
fn qwen_alt_env_var_is_discovered() {
    with_overrides(&[("QWEN_API_KEY", None), ("DASHSCOPE_API_KEY", Some("ds-key"))], || {
        assert_eq!(provider_credential_source(Provider::Qwen), Some(CredentialSource::Env));
    });
}

#[test]
fn credential_detail_surfaces_primary_env_var_name() {
    with_override("OPENROUTER_API_KEY", Some("or-key"), || {
        let detail = provider_credential_detail(Provider::OpenRouter).expect("OpenRouter discovered");
        assert_eq!(detail.source, CredentialSource::Env);
        assert_eq!(detail.env_var, Some("OPENROUTER_API_KEY"));
    });
}

#[test]
fn credential_detail_surfaces_alternate_env_var_name() {
    // When only the alternate GOOGLE_API_KEY is set, the detail must report
    // *that* name (not the primary GEMINI_API_KEY) so the UI can tell the
    // user exactly which variable was read.
    with_overrides(&[("GEMINI_API_KEY", None), ("GOOGLE_API_KEY", Some("g-key"))], || {
        let detail = provider_credential_detail(Provider::Gemini).expect("Gemini discovered");
        assert_eq!(detail.source, CredentialSource::Env);
        assert_eq!(detail.env_var, Some("GOOGLE_API_KEY"));
    });
}

#[test]
fn credential_detail_surfaces_meta_alternate_env_var_name() {
    with_overrides(&[("META_API_KEY", None), ("MODEL_API_KEY", Some("model-key"))], || {
        let detail = provider_credential_detail(Provider::Meta).expect("Meta discovered");
        assert_eq!(detail.source, CredentialSource::Env);
        assert_eq!(detail.env_var, Some("MODEL_API_KEY"));
    });
}

#[test]
fn credential_detail_env_var_is_none_for_non_env_sources() {
    // Local and managed-auth providers are discovered without an env var.
    assert_eq!(
        provider_credential_detail(Provider::Ollama).map(|d| d.env_var),
        Some(None),
        "local providers must report env_var = None"
    );
    assert_eq!(
        provider_credential_detail(Provider::Copilot).map(|d| d.env_var),
        Some(None),
        "managed-auth providers must report env_var = None"
    );
}

#[test]
fn credential_detail_returns_none_when_no_credential() {
    with_overrides(
        &[
            ("OPENROUTER_API_KEY", None),
            ("OPENAI_API_KEY", None),
            ("ANTHROPIC_API_KEY", None),
        ],
        || {
            // OpenRouter has no env var, no OAuth token in tests, no keyring
            // entry in tests → not discovered.
            assert!(provider_credential_detail(Provider::OpenRouter).is_none());
        },
    );
}

#[test]
fn discover_available_providers_carries_env_var_detail() {
    with_overrides(
        &[
            ("OPENROUTER_API_KEY", Some("or-key")),
            ("GEMINI_API_KEY", None),
            ("GOOGLE_API_KEY", Some("g-key")),
            ("OPENAI_API_KEY", None),
            ("ANTHROPIC_API_KEY", None),
        ],
        || {
            let discovered = discover_available_providers();
            let or = find_discovered(&discovered, Provider::OpenRouter).unwrap();
            assert_eq!(or.source, CredentialSource::Env);
            assert_eq!(or.env_var, Some("OPENROUTER_API_KEY"));
            let gemini = find_discovered(&discovered, Provider::Gemini).unwrap();
            assert_eq!(gemini.source, CredentialSource::Env);
            assert_eq!(gemini.env_var, Some("GOOGLE_API_KEY"));
        },
    );
}

#[test]
fn discover_available_providers_includes_ready_providers() {
    // With OPENROUTER_API_KEY set, OpenRouter must appear in discovery
    // alongside the always-ready local + managed-auth providers.
    with_overrides(
        &[
            ("OPENROUTER_API_KEY", Some("or-key")),
            ("OPENAI_API_KEY", None),
            ("ANTHROPIC_API_KEY", None),
            ("GEMINI_API_KEY", None),
        ],
        || {
            let discovered = discover_available_providers();
            let providers: Vec<Provider> = discovered.iter().map(|d| d.provider).collect();

            assert!(providers.contains(&Provider::OpenRouter), "OpenRouter should be discovered");
            assert!(providers.contains(&Provider::Ollama), "Ollama should be discovered (local)");
            assert!(providers.contains(&Provider::Copilot), "Copilot should be discovered (managed auth)");
            assert!(
                !providers.contains(&Provider::OpenAI),
                "OpenAI should NOT be discovered when OPENAI_API_KEY is unset"
            );

            let or = find_discovered(&discovered, Provider::OpenRouter).unwrap();
            assert_eq!(or.source, CredentialSource::Env);
        },
    );
}

#[test]
fn credential_source_describes_origin() {
    assert_eq!(CredentialSource::Env.describe(Provider::OpenRouter), "found in environment");
    assert_eq!(CredentialSource::Local.describe(Provider::Ollama), "local — no key required");
}

#[test]
fn get_api_key_trims_non_empty_environment_values() {
    with_override("STEPFUN_API_KEY", Some("  test-stepfun-key  "), || {
        let result = get_api_key_with_mode("stepfun", &default_sources(), crate::auth::AuthCredentialsStoreMode::File);
        assert_eq!(result.unwrap(), "test-stepfun-key");
    });
}

//! Subagent model resolution across providers, aliases and overrides.
use super::*;

#[test]
fn resolve_subagent_model_maps_aliases() {
    let cfg = DiapasonConfig::default();
    let resolved =
        resolve_subagent_model(&cfg, models::anthropic::CLAUDE_SONNET_5, "anthropic", Some("haiku"), "explorer")
            .expect("resolve model");
    assert_eq!(resolved.as_str(), models::anthropic::CLAUDE_SONNET_5);
}

#[test]
fn resolve_subagent_model_defaults_to_parent_when_omitted() {
    let cfg = DiapasonConfig::default();
    let resolved = resolve_subagent_model(&cfg, models::ollama::GPT_OSS_120B_CLOUD, "ollama", None, "worker")
        .expect("resolve model");
    assert_eq!(resolved.as_str(), models::ollama::GPT_OSS_120B_CLOUD);
}

#[test]
fn resolve_subagent_model_accepts_dotted_claude_aliases_for_anthropic() {
    let cfg = DiapasonConfig::default();
    let resolved =
        resolve_subagent_model(&cfg, "claude-haiku-4.5", "anthropic", None, "worker").expect("resolve model");
    assert_eq!(resolved.as_str(), models::anthropic::CLAUDE_SONNET_5);
}

#[test]
fn resolve_subagent_model_falls_back_to_copilot_default_for_unsupported_inherit_model() {
    let cfg = DiapasonConfig::default();
    let resolved = resolve_subagent_model(&cfg, "claude-haiku-4.5", "copilot", None, "worker").expect("resolve model");
    assert_eq!(resolved, ModelId::default_orchestrator_for_provider(Provider::Copilot));
}

#[test]
fn resolve_effective_subagent_model_uses_explicit_inherit_override() {
    // The spec names a different model, so an implementation that ignored
    // `inherit` and used the spec model would fail this assertion.
    let cfg = DiapasonConfig::default();
    let resolved = resolve_effective_subagent_model(
        &cfg,
        models::openai::GPT_5_6_SOL,
        "openai",
        Some("inherit"),
        Some(models::openai::GPT_5_6_LUNA),
        "worker",
    )
    .expect("resolve model");
    assert_eq!(resolved.as_str(), models::openai::GPT_5_6_SOL);
}

#[test]
fn resolve_effective_subagent_model_falls_back_to_parent_on_invalid_override() {
    // For non-local providers, an unrecognized override must fall back to the
    // parent model rather than being accepted as a custom identifier.
    let cfg = DiapasonConfig::default();
    let resolved = resolve_effective_subagent_model(
        &cfg,
        models::openai::GPT_5_6_SOL,
        "openai",
        Some("not-a-real-model"),
        None,
        "rust-engineer",
    )
    .expect("resolve model");
    assert_eq!(resolved.as_str(), models::openai::GPT_5_6_SOL);
}

#[test]
fn resolve_subagent_model_inherits_local_custom_model() {
    // Local providers expose arbitrary model IDs not in the built-in catalogue;
    // inheriting such a model must succeed as a custom identifier.
    let cfg = DiapasonConfig::default();
    let resolved = resolve_subagent_model(&cfg, "qwen3.5-9b-sushi-coder-rl", "lmstudio", None, "wiki-assistant")
        .expect("resolve local inherit model");
    assert_eq!(resolved.as_str(), "qwen3.5-9b-sushi-coder-rl");
    assert_eq!(resolved.provider(), Provider::LmStudio);
}

#[test]
fn resolve_subagent_model_honours_explicit_local_model() {
    let cfg = DiapasonConfig::default();
    let resolved =
        resolve_subagent_model(&cfg, "qwen3.5-9b-sushi-coder-rl", "lmstudio", Some("ornith-1.0-9b"), "wiki-assistant")
            .expect("resolve explicit local model");
    assert_eq!(resolved.as_str(), "ornith-1.0-9b");
    assert_eq!(resolved.provider(), Provider::LmStudio);
}

#[test]
fn resolve_subagent_model_honours_provider_override_model() {
    use diapason_config::core::ProviderOverrideConfig;

    let mut cfg = DiapasonConfig::default();
    cfg.provider_overrides.insert(
        "openai".to_string(),
        ProviderOverrideConfig {
            models: vec!["my-fine-tuned-gpt".to_string()],
            ..ProviderOverrideConfig::default()
        },
    );
    let resolved =
        resolve_subagent_model(&cfg, models::openai::GPT_5_6_SOL, "openai", Some("my-fine-tuned-gpt"), "reviewer")
            .expect("resolve override model");
    assert_eq!(resolved.as_str(), "my-fine-tuned-gpt");
}

#[test]
fn resolve_effective_subagent_model_ignores_cross_provider_override_model() {
    use diapason_config::core::ProviderOverrideConfig;

    // An override belonging to a DIFFERENT provider must not be accepted for the
    // active provider; resolution must fall back to the parent model instead.
    let mut cfg = DiapasonConfig::default();
    cfg.provider_overrides.insert(
        "anthropic".to_string(),
        ProviderOverrideConfig {
            models: vec!["not-a-real-model".to_string()],
            ..ProviderOverrideConfig::default()
        },
    );
    let resolved = resolve_effective_subagent_model(
        &cfg,
        models::openai::GPT_5_6_SOL,
        "openai",
        Some("not-a-real-model"),
        None,
        "reviewer",
    )
    .expect("resolve model");
    assert_eq!(resolved.as_str(), models::openai::GPT_5_6_SOL);
}

#[test]
fn resolve_subagent_model_honours_custom_provider_model() {
    use diapason_config::core::CustomProviderConfig;

    let mut cfg = DiapasonConfig::default();
    cfg.custom_providers.push(CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorp".to_string(),
        base_url: "https://llm.corp.example/v1".to_string(),
        model: "mycorp-special-coder".to_string(),
        ..CustomProviderConfig::default()
    });
    let resolved = resolve_subagent_model(&cfg, "mycorp-special-coder", "mycorp", None, "wiki-assistant")
        .expect("resolve custom provider model");
    assert_eq!(resolved.as_str(), "mycorp-special-coder");
}

#[test]
fn resolve_subagent_small_model_rejects_cross_provider_configured_lightweight_model() {
    let mut cfg = DiapasonConfig::default();
    cfg.agent.small_model.model = models::anthropic::CLAUDE_SONNET_5.to_string();

    let resolved = resolve_subagent_model(&cfg, models::openai::GPT_5_6_SOL, "openai", Some("small"), "worker")
        .expect("resolve model");

    assert_eq!(resolved, ModelId::GPT56Terra);
}

#[test]
fn resolve_effective_subagent_model_falls_back_to_spec_model_on_invalid_override() {
    // The spec model differs from the parent, so a recovery that kept the parent
    // or dropped the spec value would fail this assertion.
    let cfg = DiapasonConfig::default();
    let resolved = resolve_effective_subagent_model(
        &cfg,
        models::openai::GPT_5_6_SOL,
        "openai",
        Some("not-a-real-model"),
        Some(models::openai::GPT_5_6_LUNA),
        "reviewer",
    )
    .expect("resolve model");
    assert_eq!(resolved.as_str(), models::openai::GPT_5_6_LUNA);
}

#[test]
fn resolve_effective_subagent_model_still_errors_on_invalid_spec_model() {
    let cfg = DiapasonConfig::default();
    let err = resolve_effective_subagent_model(
        &cfg,
        models::anthropic::CLAUDE_SONNET_5,
        "anthropic",
        None,
        Some("not-a-real-model"),
        "reviewer",
    )
    .expect_err("invalid spec model should fail");
    assert!(err.to_string().contains("Failed to resolve model"));
}

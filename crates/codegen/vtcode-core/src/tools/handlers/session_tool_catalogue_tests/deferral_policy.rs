//! Deferral trigger and provider policy regressions.

use super::*;

/// Phase 7.2: the advisory warning condition for "deferred loading is
/// disabled but the catalogue would benefit from it" is pure logic in
/// `catalogue_would_benefit_from_deferral`. A disabled policy + a catalogue
/// that would defer (MCP present, over the count threshold, or over the
/// schema-token budget) is the only non-noisy warning case -- the count/
/// budget thresholds *triggering* deferral when enabled is correct
/// behaviour, not a warning condition.
#[test]
fn catalogue_would_benefit_from_deferral_detects_each_trigger() {
    // Small builtin-only catalogue: no benefit (deferral would not engage).
    assert!(
        !catalogue_would_benefit_from_deferral(false, 3, 500),
        "a small builtin-only catalogue does not benefit from deferral"
    );
    // Any MCP tool present -> benefit (MCP schemas are the dominant cost).
    assert!(catalogue_would_benefit_from_deferral(true, 1, 100), "any MCP tool means deferral would engage");
    // At the count threshold -> benefit.
    assert!(
        catalogue_would_benefit_from_deferral(false, DIRECT_TOOL_EXPOSURE_THRESHOLD, 500),
        "meeting the count threshold means deferral would engage"
    );
    // Just under the count threshold but over the token budget -> benefit
    // (the single-large-server backstop).
    assert!(
        catalogue_would_benefit_from_deferral(
            false,
            DIRECT_TOOL_EXPOSURE_THRESHOLD - 1,
            DIRECT_TOOL_EXPOSURE_TOKEN_BUDGET + 1,
        ),
        "exceeding the schema-token budget means deferral would engage"
    );
    // Exactly at the token budget (<=, not >) and under the count threshold,
    // no MCP -> no benefit (boundary matches the `<=` in `model_tools`).
    assert!(
        !catalogue_would_benefit_from_deferral(
            false,
            DIRECT_TOOL_EXPOSURE_THRESHOLD - 1,
            DIRECT_TOOL_EXPOSURE_TOKEN_BUDGET,
        ),
        "at exactly the token budget and below the count threshold, \
            deferral does not engage (boundary is <=)"
    );
}

#[test]
fn openai_policy_injects_tool_search_for_large_catalogues() {
    let exec_command = registration(tools::EXEC_COMMAND)
        .with_description("Run command")
        .with_parameter_schema(empty_object_schema());
    let mcp_tool = registration("mcp::context7::search")
        .with_catalogue_source(ToolCatalogueSource::Mcp)
        .with_llm_visibility(false)
        .with_description("search docs")
        .with_parameter_schema(empty_object_schema())
        .with_aliases(["mcp__context7__search"]);

    let mut registrations = vec![exec_command, mcp_tool];
    for index in 0..DIRECT_TOOL_EXPOSURE_THRESHOLD {
        let name: &'static str = Box::leak(format!("mcp::context7::resolve_{index}").into_boxed_str());
        let alias = format!("mcp__context7__resolve_{index}");
        registrations.push(
            registration(name)
                .with_catalogue_source(ToolCatalogueSource::Mcp)
                .with_llm_visibility(false)
                .with_description(format!("resolve docs {index}"))
                .with_parameter_schema(empty_object_schema())
                .with_aliases([alias]),
        );
    }

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(registrations);
    let definitions = catalogue.model_tools(
        SessionToolsConfig::full_public(
            SessionSurface::Interactive,
            CapabilityLevel::CodeSearch,
            ToolDocumentationMode::Full,
            ToolModelCapabilities { supports_apply_patch_tool: true },
        )
        .with_tool_profile(ToolProfile::AdvancedVtCode)
        .with_deferred_tool_policy(DeferredToolPolicy::openai_hosted(vec!["mcp__context7__search".to_string()])),
    );

    assert!(
        definitions.iter().any(|tool| tool.tool_type == "tool_search"),
        "openai hosted tool search should be injected when deferred tools exist"
    );
    let mcp_tool = definitions
        .iter()
        .find(|tool| tool.function_name() == "mcp__context7__search")
        .expect("mcp tool should be present");
    assert_eq!(mcp_tool.defer_loading, None);

    let deferred_mcp_tool = definitions
        .iter()
        .find(|tool| tool.function_name() == "mcp__context7__resolve_0")
        .expect("deferred mcp tool should be present");
    assert_eq!(deferred_mcp_tool.defer_loading, Some(true));
}

#[test]
fn openai_policy_deferred_for_small_mcp_catalogue() {
    let mcp_tool = registration("mcp::context7::search")
        .with_catalogue_source(ToolCatalogueSource::Mcp)
        .with_llm_visibility(false)
        .with_description("search docs")
        .with_parameter_schema(empty_object_schema())
        .with_aliases(["mcp__context7__search"]);
    let second_mcp_tool = registration("mcp::context7::resolve")
        .with_catalogue_source(ToolCatalogueSource::Mcp)
        .with_llm_visibility(false)
        .with_description("resolve docs")
        .with_parameter_schema(empty_object_schema())
        .with_aliases(["mcp__context7__resolve"]);

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![mcp_tool, second_mcp_tool]);
    let definitions = catalogue.model_tools(
        SessionToolsConfig::full_public(
            SessionSurface::Interactive,
            CapabilityLevel::CodeSearch,
            ToolDocumentationMode::Full,
            ToolModelCapabilities::default(),
        )
        .with_tool_profile(ToolProfile::AdvancedVtCode)
        .with_deferred_tool_policy(DeferredToolPolicy::openai_hosted(vec!["mcp__context7__search".to_string()])),
    );

    assert!(
        definitions.iter().any(|tool| tool.tool_type == "tool_search"),
        "MCP presence should trigger tool search even for a small catalogue"
    );
    let mcp_tool = definitions
        .iter()
        .find(|tool| tool.function_name() == "mcp__context7__search")
        .expect("mcp tool should be present");
    assert_eq!(mcp_tool.defer_loading, None, "always-available tool stays eager");

    let direct_mcp_tool = definitions
        .iter()
        .find(|tool| tool.function_name() == "mcp__context7__resolve")
        .expect("deferred mcp tool should be present");
    assert_eq!(direct_mcp_tool.defer_loading, Some(true), "non-always-available MCP tool should be deferred");
}

#[test]
fn always_available_tools_match_registration_names_and_aliases() {
    let mcp_tool = registration("mcp::context7::search")
        .with_catalogue_source(ToolCatalogueSource::Mcp)
        .with_llm_visibility(false)
        .with_description("search docs")
        .with_parameter_schema(empty_object_schema())
        .with_aliases(["mcp__context7__search"]);
    let dynamic_tool = registration("dynamic_skill_tool")
        .with_description("dynamic skill tool")
        .with_parameter_schema(empty_object_schema());

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![mcp_tool, dynamic_tool]);
    let definitions = catalogue.model_tools(
        SessionToolsConfig::full_public(
            SessionSurface::Interactive,
            CapabilityLevel::CodeSearch,
            ToolDocumentationMode::Full,
            ToolModelCapabilities::default(),
        )
        .with_tool_profile(ToolProfile::AdvancedVtCode)
        .with_deferred_tool_policy(DeferredToolPolicy::openai_hosted(vec![
            "mcp::context7::search".to_string(),
            "dynamic_skill_tool".to_string(),
        ])),
    );

    let mcp_tool = definitions
        .iter()
        .find(|tool| tool.function_name() == "mcp__context7__search")
        .expect("mcp tool should be present");
    assert_eq!(mcp_tool.defer_loading, None);

    let dynamic_tool = definitions
        .iter()
        .find(|tool| tool.function_name() == "dynamic_skill_tool")
        .expect("dynamic tool should be present");
    assert_eq!(dynamic_tool.defer_loading, None);
}

#[test]
fn unsupported_providers_keep_catalogue_eager() {
    let exec_command = registration(tools::EXEC_COMMAND)
        .with_description("Run command")
        .with_parameter_schema(empty_object_schema());
    let mcp_tool = registration("mcp::context7::search")
        .with_catalogue_source(ToolCatalogueSource::Mcp)
        .with_llm_visibility(false)
        .with_description("search docs")
        .with_parameter_schema(empty_object_schema())
        .with_aliases(["mcp__context7__search"]);

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![exec_command, mcp_tool]);
    let definitions = catalogue.model_tools(
        SessionToolsConfig::full_public(
            SessionSurface::Interactive,
            CapabilityLevel::CodeSearch,
            ToolDocumentationMode::Full,
            ToolModelCapabilities::default(),
        )
        .with_tool_profile(ToolProfile::AdvancedVtCode),
    );

    assert!(!definitions.iter().any(|tool| tool.is_tool_search()));
    assert!(
        definitions.iter().all(|tool| tool.defer_loading.is_none()),
        "unsupported providers should keep the eager catalogue"
    );
}

#[test]
fn deferred_tool_policy_uses_provider_defaults() {
    let config = VTCodeConfig::default();

    let anthropic = deferred_tool_policy_for_runtime(Some(Provider::Anthropic), false, Some(&config));
    assert!(anthropic.is_enabled());
    assert_eq!(
        anthropic.tool_search_definition().map(|tool| tool.tool_type),
        Some("tool_search_tool_regex_20251119".to_string())
    );

    let openai = deferred_tool_policy_for_runtime(Some(Provider::OpenAI), true, Some(&config));
    assert!(openai.is_enabled());
    assert_eq!(openai.tool_search_definition().map(|tool| tool.tool_type), Some("tool_search".to_string()));

    // OpenAI without Responses compaction, and no explicit provider-hosted
    // tool search, falls through to client-local deferral now that
    // `client_tool_search` defaults to `true`.
    let unsupported = deferred_tool_policy_for_runtime(Some(Provider::OpenAI), false, Some(&config));
    assert!(unsupported.is_enabled());
    assert!(unsupported.is_client_local());
}

#[test]
fn client_local_policy_selected_when_flag_enabled_for_unsupported_provider() {
    let mut config = VTCodeConfig::default();
    config.tools.client_tool_search = true;

    let gemini = deferred_tool_policy_for_runtime(Some(Provider::Gemini), false, Some(&config));
    assert!(gemini.is_enabled());
    assert!(gemini.is_client_local());
    assert_eq!(gemini.tool_search_definition(), None);

    // No provider inferred (e.g. unknown/custom model) is also covered
    // by the fallthrough arm.
    let no_provider = deferred_tool_policy_for_runtime(None, false, Some(&config));
    assert!(no_provider.is_enabled());
    assert!(no_provider.is_client_local());
}

#[test]
fn client_local_policy_not_selected_when_flag_disabled() {
    let mut config = VTCodeConfig::default();
    // Default is enabled; explicitly disable it to test the fallback path.
    config.tools.client_tool_search = false;
    assert!(!config.tools.client_tool_search);

    let gemini = deferred_tool_policy_for_runtime(Some(Provider::Gemini), false, Some(&config));
    assert!(!gemini.is_enabled());
    assert!(!gemini.is_client_local());

    let no_config = deferred_tool_policy_for_runtime(Some(Provider::Gemini), false, None);
    assert!(!no_config.is_enabled());
    assert!(!no_config.is_client_local());
}

#[test]
fn anthropic_native_memory_runtime_flag_tracks_provider_and_config() {
    let mut config = VTCodeConfig::default();
    config.provider.anthropic.memory.enabled = true;

    assert!(anthropic_native_memory_enabled_for_runtime(
        Some(Provider::Anthropic),
        "claude-sonnet-5",
        Some(&config),
    ));
    assert!(!anthropic_native_memory_enabled_for_runtime(
        Some(Provider::OpenAI),
        "claude-sonnet-5",
        Some(&config),
    ));
    assert!(!anthropic_native_memory_enabled_for_runtime(Some(Provider::Anthropic), "gpt-5", Some(&config),));
    assert!(anthropic_native_memory_enabled_for_runtime(
        Some(Provider::Anthropic),
        "my-private-claude-build",
        Some(&config),
    ));
}

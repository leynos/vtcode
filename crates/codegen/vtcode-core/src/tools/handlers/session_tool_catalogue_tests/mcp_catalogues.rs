//! MCP namespace and deferred catalogue regressions.

use super::*;

#[test]
fn mcp_tool_registration_derives_namespace_from_server_name() {
    let mcp_tool = registration("mcp::context7::search")
        .with_catalogue_source(ToolCatalogueSource::Mcp)
        .with_llm_visibility(false)
        .with_description("search docs")
        .with_parameter_schema(empty_object_schema())
        .with_aliases(["mcp__context7__search"]);

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![mcp_tool]);
    let entry = catalogue
        .entries()
        .iter()
        .find(|entry| entry.public_name == "mcp__context7__search")
        .expect("mcp entry should be present");

    let namespace = entry
        .namespace
        .as_ref()
        .expect("mcp tool should derive a namespace from its server name");
    assert_eq!(namespace.name, "context7");
    assert_eq!(namespace.description, "Tools provided by MCP server 'context7'");
}

#[test]
fn core_tool_registration_has_no_namespace() {
    let exec_command = registration(tools::EXEC_COMMAND)
        .with_description("Run command")
        .with_parameter_schema(empty_object_schema());

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![exec_command]);
    let entry = catalogue
        .entries()
        .iter()
        .find(|entry| entry.public_name == tools::EXEC_COMMAND)
        .expect("core tool entry should be present");

    assert!(entry.namespace.is_none(), "core/builtin tools should not derive a namespace");
}

#[test]
fn model_tools_attach_namespace_only_to_deferred_mcp_tools() {
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
            ToolModelCapabilities::default(),
        )
        .with_tool_profile(ToolProfile::AdvancedVtCode)
        .with_deferred_tool_policy(DeferredToolPolicy::anthropic(ToolSearchAlgorithm::Regex, Vec::new())),
    );

    let core_tool = definitions
        .iter()
        .find(|tool| tool.function_name() == tools::EXEC_COMMAND)
        .expect("exec_command should be present");
    assert_eq!(core_tool.defer_loading, None);
    assert!(core_tool.namespace.is_none(), "non-deferred core tools should never carry namespace metadata");

    let deferred_mcp_tool = definitions
        .iter()
        .find(|tool| tool.function_name() == "mcp__context7__search")
        .expect("deferred mcp tool should be present");
    assert_eq!(deferred_mcp_tool.defer_loading, Some(true));
    let namespace = deferred_mcp_tool
        .namespace
        .as_ref()
        .expect("deferred mcp tool should carry namespace metadata");
    assert_eq!(namespace.name, "context7");
}

#[test]
fn small_mcp_catalogue_is_deferred_despite_low_tool_count() {
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
        .with_tool_profile(ToolProfile::AdvancedVtCode)
        .with_deferred_tool_policy(DeferredToolPolicy::anthropic(ToolSearchAlgorithm::Regex, Vec::new())),
    );

    let mcp_definition = definitions
        .iter()
        .find(|tool| tool.function_name() == "mcp__context7__search")
        .expect("mcp tool should be present");
    assert_eq!(
        mcp_definition.defer_loading,
        Some(true),
        "even a single MCP tool should be deferred to avoid schema tax"
    );
}

#[test]
fn client_local_policy_deferred_for_small_mcp_catalogue() {
    let exec_command = registration(tools::EXEC_COMMAND)
        .with_description("Run command")
        .with_parameter_schema(empty_object_schema());
    let mcp_search_tools = registration(tools::MCP_SEARCH_TOOLS)
        .with_description("Search MCP tools")
        .with_parameter_schema(empty_object_schema());
    let mcp_tool = registration("mcp::context7::search")
        .with_catalogue_source(ToolCatalogueSource::Mcp)
        .with_llm_visibility(false)
        .with_description("search docs")
        .with_parameter_schema(empty_object_schema())
        .with_aliases(["mcp__context7__search"]);

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![exec_command, mcp_search_tools, mcp_tool]);
    let definitions = catalogue.model_tools(
        SessionToolsConfig::full_public(
            SessionSurface::Interactive,
            CapabilityLevel::CodeSearch,
            ToolDocumentationMode::Full,
            ToolModelCapabilities::default(),
        )
        .with_tool_profile(ToolProfile::AdvancedVtCode)
        .with_deferred_tool_policy(DeferredToolPolicy::client_local(Vec::new())),
    );

    assert!(
        definitions.iter().any(|tool| tool.function_name() == "mcp__context7__search"),
        "mcp tool should still be listed in the model-facing catalogue for client-local search"
    );
    let mcp_definition = definitions
        .iter()
        .find(|tool| tool.function_name() == "mcp__context7__search")
        .expect("mcp tool should be present");
    assert_eq!(
        mcp_definition.defer_loading,
        Some(true),
        "client-local deferral should also apply to small MCP catalogues"
    );
    let search_definition = definitions
        .iter()
        .find(|tool| tool.function_name() == tools::MCP_SEARCH_TOOLS)
        .expect("client-local MCP search should remain available");
    assert_eq!(search_definition.defer_loading, None);
}

#[test]
fn client_local_policy_exposes_small_builtin_catalogue_directly() {
    // Plan-mode policy filtering shrinks the visible catalogue to a handful
    // of read-only builtins. Deferring that tiny set drops every tool from
    // the wire payload (on_wire_tools = 0), so the model improvises
    // textual XML tool calls instead of native ones (turn_887/turn_888).
    // Small catalogues must stay eager: deferral exists to shed schema tax,
    // not to hide the whole catalogue.
    let exec_command = registration(tools::EXEC_COMMAND)
        .with_description("Run command")
        .with_parameter_schema(empty_object_schema());
    let code_search = registration(tools::CODE_SEARCH)
        .with_description("Search code")
        .with_parameter_schema(empty_object_schema());
    let read_file = registration(tools::READ_FILE)
        .with_description("Read file")
        .with_parameter_schema(empty_object_schema());

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![exec_command, code_search, read_file]);
    let definitions = catalogue.model_tools(
        SessionToolsConfig::full_public(
            SessionSurface::Interactive,
            CapabilityLevel::CodeSearch,
            ToolDocumentationMode::Full,
            ToolModelCapabilities::default(),
        )
        .with_tool_profile(ToolProfile::AdvancedVtCode)
        .with_deferred_tool_policy(DeferredToolPolicy::client_local(Vec::new())),
    );

    assert!(
        definitions.iter().all(|tool| tool.defer_loading.is_none()),
        "a small builtin-only catalogue gains nothing from client-local deferral; every tool must stay on the wire"
    );
}

#[test]
fn client_local_policy_defers_large_builtin_catalogue() {
    let exec_command = registration(tools::EXEC_COMMAND)
        .with_description("Run command")
        .with_parameter_schema(empty_object_schema());
    let code_search = registration(tools::CODE_SEARCH)
        .with_description("Search code")
        .with_parameter_schema(empty_object_schema());

    let mut registrations = vec![exec_command, code_search];
    for index in 0..DIRECT_TOOL_EXPOSURE_THRESHOLD {
        let name: &'static str = Box::leak(format!("extra_builtin_{index}").into_boxed_str());
        registrations.push(
            registration(name)
                .with_description(format!("extra builtin {index}"))
                .with_parameter_schema(empty_object_schema()),
        );
    }

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(registrations);
    let definitions = catalogue.model_tools(
        SessionToolsConfig::full_public(
            SessionSurface::Interactive,
            CapabilityLevel::CodeSearch,
            ToolDocumentationMode::Full,
            ToolModelCapabilities::default(),
        )
        .with_tool_profile(ToolProfile::AdvancedVtCode)
        .with_deferred_tool_policy(DeferredToolPolicy::client_local(Vec::new())),
    );

    let exec_tool = definitions
        .iter()
        .find(|tool| tool.function_name() == tools::EXEC_COMMAND)
        .expect("exec_command should be present");
    assert_eq!(exec_tool.defer_loading, None, "core tools stay eager even in large catalogues");
    let search_tool = definitions
        .iter()
        .find(|tool| tool.function_name() == tools::CODE_SEARCH)
        .expect("code_search should be present");
    assert_eq!(
        search_tool.defer_loading,
        Some(true),
        "large builtin catalogues still defer non-core tools under client-local policy"
    );
}

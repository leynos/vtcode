//! Planning schema token and MCP first-request regressions.

use super::*;

/// Serialize the on-wire tool schemas (the definitions the model actually
/// receives) and estimate their token cost at ~4 chars/token, matching the
/// convention in `estimate_schema_tokens` and the first-request budget test
/// in `tools/registry/builtins.rs`. Deferred tools (`defer_loading ==
/// Some(true)`) are omitted under the client-local policy, so they never
/// reach the wire payload and are excluded here.
fn on_wire_schema_tokens(catalogue: &SessionToolCatalogue, config: SessionToolsConfig) -> usize {
    #[derive(Serialize)]
    struct Estimate<'a> {
        name: &'a str,
        description: &'a str,
        parameters: &'a Value,
    }
    let on_wire: FxHashSet<String> = catalogue
        .model_tools(config.clone())
        .into_iter()
        .filter(|tool| tool.defer_loading != Some(true))
        .map(|tool| tool.function_name().to_string())
        .collect();
    catalogue
        .schema_entries(config)
        .into_iter()
        .filter(|entry| on_wire.contains(&entry.name))
        .map(|entry| {
            serde_json::to_string(&Estimate {
                name: &entry.name,
                description: &entry.description,
                parameters: &entry.parameters,
            })
            .map(|s| s.len() / 4)
            .unwrap_or(0)
        })
        .sum()
}

#[test]
fn task_tracker_schema_token_estimate_tracks_workflow_specific_parameters() {
    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![
        registration(tools::TASK_TRACKER)
            .with_description("Track plan tasks")
            .with_parameter_schema(empty_object_schema()),
    ]);
    let base_config = SessionToolsConfig::full_public(
        SessionSurface::Interactive,
        CapabilityLevel::CodeSearch,
        ToolDocumentationMode::Full,
        ToolModelCapabilities::default(),
    )
    .with_tool_profile(ToolProfile::AdvancedVtCode);

    let standard_config = base_config.clone().with_planning_active(false);
    let standard_visible = catalogue.visible_entry_indices(&standard_config);
    assert_eq!(
        catalogue.estimate_schema_tokens(&standard_visible, &standard_config),
        on_wire_schema_tokens(&catalogue, standard_config),
        "inactive task_tracker token estimate should match the emitted schema",
    );

    let planning_config = base_config.with_planning_active(true);
    let planning_visible = catalogue.visible_entry_indices(&planning_config);
    assert_eq!(
        catalogue.estimate_schema_tokens(&planning_visible, &planning_config),
        on_wire_schema_tokens(&catalogue, planning_config),
        "planning task_tracker token estimate should match the emitted schema",
    );
}

#[test]
fn task_tracker_schema_keeps_max_output_tokens_in_standard_and_planning_modes() {
    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![
        registration(tools::TASK_TRACKER)
            .with_description("Track plan tasks")
            .with_parameter_schema(empty_object_schema()),
    ]);
    let base_config = SessionToolsConfig::full_public(
        SessionSurface::Interactive,
        CapabilityLevel::CodeSearch,
        ToolDocumentationMode::Full,
        ToolModelCapabilities::default(),
    )
    .with_tool_profile(ToolProfile::AdvancedVtCode);

    for planning_active in [false, true] {
        let function = catalogue
            .model_tools(base_config.clone().with_planning_active(planning_active))
            .into_iter()
            .find_map(|tool| (tool.function_name() == tools::TASK_TRACKER).then_some(tool.function).flatten())
            .unwrap_or_else(|| panic!("missing task_tracker function for planning_active={planning_active}"));

        assert_eq!(
            function.parameters["properties"]["max_output_tokens"]["default"],
            json!(vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS),
            "task_tracker schema must keep max_output_tokens when planning_active={planning_active}",
        );
    }
}

/// Build a simulated MCP tool registration for `server`/`tool` with a
/// realistic one-line description and a two-parameter schema, so the eager
/// schema tax is pronounced enough to assert on.
fn mcp_server_tool_registration(server: &str, tool: &str, description: &str) -> ToolRegistration {
    let name: &'static str = Box::leak(format!("mcp::{server}::{tool}").into_boxed_str());
    let alias = format!("mcp__{server}__{tool}");
    registration(name)
        .with_catalogue_source(ToolCatalogueSource::Mcp)
        .with_llm_visibility(false)
        .with_description(description.to_string())
        .with_parameter_schema(json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The lookup query." },
                "limit": { "type": "integer", "description": "Max results to return." }
            },
            "required": ["query"]
        }))
        .with_aliases([alias])
}

/// Phase 1.1 / 2.x regression: with several MCP servers attached, the
/// client-local deferred-loading path must keep the first-request wire
/// payload near the no-MCP baseline (because MCP schemas are omitted),
/// while eager exposure would balloon it. This proves the win the
/// `DIRECT_TOOL_EXPOSURE_THRESHOLD` / `has_mcp_tools` gating delivers.
#[test]
fn mcp_deferral_keeps_first_request_wire_payload_near_baseline() {
    let core_registrations = || {
        vec![
            registration(tools::EXEC_COMMAND)
                .with_description("Run a shell command in the workspace sandbox.")
                .with_parameter_schema(empty_object_schema()),
            registration(tools::APPLY_PATCH)
                .with_llm_visibility(false)
                .with_description("Apply a structured patch to files.")
                .with_parameter_schema(apply_patch_parameters())
                .with_behaviour(ToolBehaviour::apply_patch(ToolMutationModel::Mutating, false, true)),
            registration(tools::MCP_SEARCH_TOOLS)
                .with_description("Search across deferred MCP tools by keyword.")
                .with_parameter_schema(empty_object_schema()),
            registration(tools::CODE_SEARCH)
                .with_description("Search the codebase symbol index.")
                .with_parameter_schema(empty_object_schema()),
        ]
    };

    // Five simulated MCP servers, four tools each = 20 deferred MCP tools.
    let mut registrations = core_registrations();
    for server in ["context7", "filesystem", "github", "slack", "postgres"] {
        for index in 0..4 {
            let tool = format!("op_{index}");
            let description =
                format!("{server} operation {index}: query and mutate {server} resources with paging and filters.");
            registrations.push(mcp_server_tool_registration(server, &tool, &description));
        }
    }
    let catalogue = SessionToolCatalogue::rebuild_from_registrations(registrations);

    let make_config = |policy: DeferredToolPolicy| {
        SessionToolsConfig::full_public(
            SessionSurface::Interactive,
            CapabilityLevel::CodeSearch,
            ToolDocumentationMode::Full,
            ToolModelCapabilities::default(),
        )
        .with_tool_profile(ToolProfile::AdvancedVtCode)
        .with_deferred_tool_policy(policy)
    };

    // Eager: no deferral, so all 20 MCP schemas travel on the first request.
    let eager_tokens = on_wire_schema_tokens(&catalogue, make_config(DeferredToolPolicy::default()));
    // Client-local deferral: MCP tools are omitted from the wire payload.
    let deferred_tokens = on_wire_schema_tokens(&catalogue, make_config(DeferredToolPolicy::client_local(Vec::new())));
    // Baseline: the same core tools, no MCP servers, under deferral.
    let baseline_catalogue = SessionToolCatalogue::rebuild_from_registrations(core_registrations());
    let baseline_tokens =
        on_wire_schema_tokens(&baseline_catalogue, make_config(DeferredToolPolicy::client_local(Vec::new())));

    assert!(
        eager_tokens >= deferred_tokens * 3,
        "eager payload ({eager_tokens}) must be at least 3x the deferred payload \
             ({deferred_tokens}) -- otherwise the MCP schema tax deferral removes is not real"
    );
    assert!(
        deferred_tokens <= baseline_tokens * 5 / 4,
        "with 5 MCP servers under deferral, the first-request payload ({deferred_tokens}) \
             must stay within 25% of the no-MCP baseline ({baseline_tokens})"
    );
    assert!(
        deferred_tokens < eager_tokens,
        "deferral must shrink the first-request wire payload: \
             eager={eager_tokens} deferred={deferred_tokens}"
    );
}

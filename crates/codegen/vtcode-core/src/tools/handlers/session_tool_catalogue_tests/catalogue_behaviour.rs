//! Schema visibility and catalogue behaviour regressions.

use super::schema_projections::contains_json_key;
use super::*;

#[test]
fn schema_entries_hide_request_user_input_when_disabled() {
    let registration = registration(tools::REQUEST_USER_INPUT)
        .with_description("Ask the user")
        .with_parameter_schema(empty_object_schema());

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![registration]);
    let names = catalogue.public_tool_names(SessionToolsConfig {
        surface: SessionSurface::Interactive,
        capability_level: CapabilityLevel::CodeSearch,
        documentation_mode: ToolDocumentationMode::Full,
        planning_active: true,
        request_user_input_enabled: false,
        model_capabilities: ToolModelCapabilities::default(),
        deferred_tool_policy: DeferredToolPolicy::default(),
        anthropic_native_memory_enabled: false,
        tool_profile: ToolProfile::VtCode,
    });

    assert!(names.is_empty());
}

#[test]
fn task_tracker_stays_visible_outside_planning_workflow() {
    let registration = registration(tools::TASK_TRACKER)
        .with_description("Track plan tasks")
        .with_parameter_schema(empty_object_schema());

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![registration]);
    let names = catalogue.public_tool_names(SessionToolsConfig {
        surface: SessionSurface::Interactive,
        capability_level: CapabilityLevel::CodeSearch,
        documentation_mode: ToolDocumentationMode::Full,
        planning_active: false,
        request_user_input_enabled: true,
        model_capabilities: ToolModelCapabilities::default(),
        deferred_tool_policy: DeferredToolPolicy::default(),
        anthropic_native_memory_enabled: false,
        tool_profile: ToolProfile::AdvancedVtCode,
    });

    assert_eq!(names, vec![tools::TASK_TRACKER.to_string()]);
}

#[test]
fn memory_tool_is_hidden_unless_anthropic_native_memory_is_enabled() {
    let registration = registration(tools::MEMORY)
        .with_description("Native memory")
        .with_parameter_schema(empty_object_schema());
    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![registration]);

    let hidden = catalogue.public_tool_names(SessionToolsConfig::full_public(
        SessionSurface::Interactive,
        CapabilityLevel::CodeSearch,
        ToolDocumentationMode::Full,
        ToolModelCapabilities::default(),
    ));
    assert!(hidden.is_empty());

    let visible = catalogue.public_tool_names(
        SessionToolsConfig::full_public(
            SessionSurface::Interactive,
            CapabilityLevel::CodeSearch,
            ToolDocumentationMode::Full,
            ToolModelCapabilities::default(),
        )
        .with_tool_profile(ToolProfile::AdvancedVtCode)
        .with_anthropic_native_memory_enabled(true),
    );
    assert_eq!(visible, vec![tools::MEMORY.to_string()]);
}

#[test]
fn memory_tool_uses_anthropic_native_definition_when_visible() {
    let registration = registration(tools::MEMORY)
        .with_description("Native memory")
        .with_parameter_schema(empty_object_schema());
    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![registration]);

    let definitions = catalogue.model_tools(
        SessionToolsConfig::full_public(
            SessionSurface::Interactive,
            CapabilityLevel::CodeSearch,
            ToolDocumentationMode::Full,
            ToolModelCapabilities::default(),
        )
        .with_tool_profile(ToolProfile::AdvancedVtCode)
        .with_anthropic_native_memory_enabled(true),
    );

    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].tool_type, "memory_20250818");
    assert_eq!(definitions[0].function_name(), tools::MEMORY);
}

#[test]
fn apply_patch_uses_special_tool_when_supported() {
    let registration = registration(tools::APPLY_PATCH)
        .with_llm_visibility(false)
        .with_description("Apply patch")
        .with_parameter_schema(apply_patch_parameters())
        .with_behaviour(ToolBehaviour::apply_patch(ToolMutationModel::Mutating, false, true));

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![registration]);
    let tools = catalogue.model_tools(SessionToolsConfig::full_public(
        SessionSurface::Interactive,
        CapabilityLevel::CodeSearch,
        ToolDocumentationMode::Full,
        ToolModelCapabilities { supports_apply_patch_tool: true },
    ));

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_type, "apply_patch");
}

#[test]
fn apply_patch_falls_back_to_function_tool_when_unsupported() {
    let registration = registration(tools::APPLY_PATCH)
        .with_llm_visibility(false)
        .with_description("Apply patch")
        .with_parameter_schema(apply_patch_parameters())
        .with_behaviour(ToolBehaviour::apply_patch(ToolMutationModel::Mutating, false, true));

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![registration]);
    let tools = catalogue.model_tools(SessionToolsConfig::full_public(
        SessionSurface::Interactive,
        CapabilityLevel::CodeSearch,
        ToolDocumentationMode::Full,
        ToolModelCapabilities::default(),
    ));

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].tool_type, "function");
}

#[test]
fn agent_runner_default_hides_legacy_browse_tools() {
    let read_file = registration(tools::READ_FILE)
        .with_llm_visibility(false)
        .with_description("Read file contents in chunks")
        .with_parameter_schema(empty_object_schema());
    let list_files = registration(tools::LIST_FILES)
        .with_llm_visibility(false)
        .with_description("List files with pagination")
        .with_parameter_schema(list_files_parameters());
    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![read_file, list_files]);

    let interactive_names = catalogue.public_tool_names(SessionToolsConfig::full_public(
        SessionSurface::Interactive,
        CapabilityLevel::CodeSearch,
        ToolDocumentationMode::Full,
        ToolModelCapabilities::default(),
    ));
    assert!(!interactive_names.contains(&tools::READ_FILE.to_string()));
    assert!(!interactive_names.contains(&tools::LIST_FILES.to_string()));

    let agent_runner_names = catalogue.public_tool_names(SessionToolsConfig::full_public(
        SessionSurface::AgentRunner,
        CapabilityLevel::CodeSearch,
        ToolDocumentationMode::Full,
        ToolModelCapabilities::default(),
    ));
    assert!(!agent_runner_names.contains(&tools::READ_FILE.to_string()));
    assert!(!agent_runner_names.contains(&tools::LIST_FILES.to_string()));
}

#[test]
fn parallel_support_comes_from_behaviour_metadata() {
    let registration = registration("parallel_catalogue_tool")
        .with_description("parallel-safe test tool")
        .with_parameter_schema(empty_object_schema())
        .with_behaviour(ToolBehaviour::function(ToolMutationModel::ReadOnly, true, false));

    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![registration]);
    assert_eq!(catalogue.entries().len(), 1);
    assert!(catalogue.entries()[0].supports_parallel_tool_calls);
}

#[test]
fn model_tool_serialization_keeps_output_cap_separate_from_approval_policy() {
    let registration = registration(tools::EXEC_COMMAND)
        .with_description("Run the policy surface test")
        .with_parameter_schema(empty_object_schema())
        .with_permission(ToolPolicy::Allow);
    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![registration]);
    let definitions = catalogue.model_tools(
        SessionToolsConfig::full_public(
            SessionSurface::Interactive,
            CapabilityLevel::CodeSearch,
            ToolDocumentationMode::Full,
            ToolModelCapabilities::default(),
        )
        .with_tool_profile(ToolProfile::AdvancedVtCode),
    );
    let tool = definitions.first().expect("model tool definition");
    let serialized = serde_json::to_value(tool).expect("serialize model tool definition");

    assert_eq!(
        tool.function.as_ref().map(|function| function.description.as_str()),
        Some("Run the policy surface test")
    );
    assert_eq!(
        tool.function
            .as_ref()
            .map(|function| function.parameters["properties"]["max_output_tokens"]["default"].clone()),
        Some(json!(vtcode_utility_tool_specs::DEFAULT_MAX_OUTPUT_TOKENS))
    );
    for key in [
        "approval_policy",
        "default_permission",
        "permission",
        "tool_policy",
        "allow_patterns",
        "deny_patterns",
    ] {
        assert!(!contains_json_key(&serialized, key), "approval metadata leaked into model schema: {key}");
    }
}

//! Tests for session tool-catalogue projection, schema, and policy behaviour.
//!
//! Child files keep their own test bodies; registrations shared by the
//! default-profile regressions live in `default_profile_registrations`.

use super::*;
use crate::config::DiapasonConfig;
use crate::tools::constants::empty_object_schema;
use crate::tools::registry::ToolRegistration;
use crate::tools::request_user_input::RequestUserInputTool;
use crate::tools::tool_intent::{ToolBehaviour, ToolMutationModel};
use crate::tools::traits::Tool;
use rstest::fixture;
use serde_json::json;

fn registration(name: &'static str) -> ToolRegistration {
    ToolRegistration::new(name, CapabilityLevel::CodeSearch, false, |_, _| Box::pin(async { Ok(Value::Null) }))
}

/// Registrations shared by the default-profile regressions, identical in
/// both callers. `READ_FILE` deliberately stays test-local because its
/// `llm_visibility` differs between them.
#[fixture]
fn default_profile_registrations() -> Vec<ToolRegistration> {
    vec![
        registration(tools::EXEC_COMMAND)
            .with_description("Run command")
            .with_parameter_schema(empty_object_schema()),
        registration(tools::WRITE_STDIN)
            .with_description("Write stdin")
            .with_parameter_schema(empty_object_schema()),
        registration(tools::APPLY_PATCH)
            .with_llm_visibility(false)
            .with_description("Apply patch")
            .with_parameter_schema(apply_patch_parameters())
            .with_behaviour(ToolBehaviour::apply_patch(ToolMutationModel::Mutating, false, true)),
        registration(tools::CODE_SEARCH)
            .with_description("Search code")
            .with_parameter_schema(empty_object_schema()),
        registration(tools::SEARCH_TOOLS)
            .with_description("Discover deferred tools")
            .with_parameter_schema(empty_object_schema()),
    ]
}

fn public_names_for_single_tool(name: &'static str, surface: SessionSurface) -> Vec<String> {
    let registration = registration(name)
        .with_description("Test tool")
        .with_parameter_schema(empty_object_schema());
    let catalogue = SessionToolCatalogue::rebuild_from_registrations(vec![registration]);
    catalogue.public_tool_names(SessionToolsConfig::full_public(
        surface,
        CapabilityLevel::CodeSearch,
        ToolDocumentationMode::Full,
        ToolModelCapabilities::default(),
    ))
}

mod catalogue_behaviour;
mod catalogue_surface;
mod default_profiles;
mod deferral_policy;
mod mcp_catalogues;
mod planning_schema;
mod schema_projections;

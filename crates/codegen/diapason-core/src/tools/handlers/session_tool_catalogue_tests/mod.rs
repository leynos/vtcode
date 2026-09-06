//! Tests for session tool-catalogue projection, schema, and policy behaviour.
//!
//! Child files retain their original test and fixture blocks byte-for-byte.

use super::*;
use crate::config::DiapasonConfig;
use crate::tools::constants::empty_object_schema;
use crate::tools::registry::ToolRegistration;
use crate::tools::request_user_input::RequestUserInputTool;
use crate::tools::tool_intent::{ToolBehaviour, ToolMutationModel};
use crate::tools::traits::Tool;
use serde_json::json;

fn registration(name: &'static str) -> ToolRegistration {
    ToolRegistration::new(name, CapabilityLevel::CodeSearch, false, |_, _| Box::pin(async { Ok(Value::Null) }))
}

fn public_names_for_single_tool(name: &'static str, surface: SessionSurface) -> Vec<String> {
    let registration = registration(name)
        .with_description("Test tool")
        .with_parameter_schema(empty_object_schema());
    let catalog = SessionToolCatalogue::rebuild_from_registrations(vec![registration]);
    catalog.public_tool_names(SessionToolsConfig::full_public(
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

//! Tests for session tool-catalogue projection, schema, and policy behaviour.
//!
//! Child files retain their original test and fixture blocks byte-for-byte.

use super::*;
use crate::config::VTCodeConfig;
use crate::tools::constants::empty_object_schema;
use crate::tools::registry::ToolRegistration;
use crate::tools::request_user_input::RequestUserInputTool;
use crate::tools::tool_intent::{ToolBehaviour, ToolMutationModel};
use crate::tools::traits::Tool;
use serde_json::json;

fn registration(name: &'static str) -> ToolRegistration {
    ToolRegistration::new(name, CapabilityLevel::CodeSearch, false, |_, _| Box::pin(async { Ok(Value::Null) }))
}

mod catalogue_behaviour;
mod catalogue_surface;
mod default_profiles;
mod deferral_policy;
mod mcp_catalogues;
mod planning_schema;
mod schema_projections;

//! Compile-pass guards for the renamed catalogue prompt APIs.
//!
//! Each assertion binds a replacement API to an explicit function-pointer
//! type. The test fails to compile if the API is removed or its signature
//! changes, so the `catalog` to `catalogue` rename cannot silently regress.
use super::*;
use crate::core::agent::hash_utils::stable_system_prefix_hash;

#[test]
fn replacement_catalogue_prompt_apis_keep_their_signatures() {
    let _: fn(&mut String, &SessionToolCatalogueSnapshot, bool) = append_runtime_tool_prompt_sections;
    let _: fn(&mut String, &SessionToolCatalogueSnapshot, bool, ResolvedShellPromptProfile) =
        append_runtime_tool_prompt_sections_for_profile;
    let _: fn(&mut String, &[ToolDefinition]) = append_deferred_tools_prompt_section;
    let _: fn(&str) -> u64 = stable_system_prefix_hash;
}

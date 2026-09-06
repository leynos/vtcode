//! System-prompt tests grouped by composition concern.

use super::*;
use crate::config::VTCodeConfig;
use crate::config::constants::tools;
use crate::config::types::ResolvedShellPromptProfile;
use std::path::PathBuf;

const REMOVED_MODEL_FACING_TOOL_NAMES: &[&str] = &[
    "command_session",
    "file_operation",
    "search_dispatch",
    "list_files",
    "read_file",
    "write_file",
    "edit_file",
    "grep_file",
];

fn assert_no_removed_model_facing_tool_names(prompt: &str) {
    for tool_name in REMOVED_MODEL_FACING_TOOL_NAMES {
        assert!(!prompt.contains(tool_name), "prompt should not mention removed tool name {tool_name}");
    }
}

mod budget_and_cache;
mod dynamic_resources;
mod environment_layers;
mod identity_and_static;
mod mode_selection;
mod planning_contract;
mod prompt_contract;

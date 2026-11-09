//! Tool catalogue advertised to Agent Client Protocol (ACP) clients.
//!
//! Builds ACP tool definitions from the shared schemas and display titles, and
//! maps tool names to either a native ACP tool or a locally registered
//! definition so callers can dispatch lookups uniformly.

use diapason_core::config::constants::tools;
use diapason_core::llm::provider::ToolDefinition;
use hashbrown::HashMap;
use serde_json::Value;
use std::path::Path;

use super::schemas::{build_list_files_definition, build_read_file_definition};
use super::titles::render_title;

/// Enum of tools available via the Agent Client Protocol (ACP) integration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SupportedTool {
    ReadFile,
    ListFiles,
}

impl SupportedTool {
    /// ACP tool kind advertised for this tool.
    pub(crate) fn kind(&self) -> crate::acp::ToolKind {
        match self {
            Self::ReadFile => crate::acp::ToolKind::Read,
            Self::ListFiles => crate::acp::ToolKind::Search,
        }
    }

    /// Human-readable title used when a call supplies no arguments to
    /// specialise.
    pub(crate) fn default_title(&self) -> &'static str {
        match self {
            Self::ReadFile => "Read file",
            Self::ListFiles => "List files",
        }
    }

    /// Provider-facing function name registered for this tool.
    pub fn function_name(&self) -> &'static str {
        match self {
            Self::ReadFile => tools::READ_FILE,
            Self::ListFiles => tools::LIST_FILES,
        }
    }

    /// Stable ordering key that fixes the advertised tool order.
    fn sort_key(&self) -> u8 {
        match self {
            Self::ReadFile => 0,
            Self::ListFiles => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolDescriptor {
    Acp(SupportedTool),
    Local,
}

impl ToolDescriptor {
    /// ACP tool kind for this descriptor; locally registered tools report
    /// [`crate::acp::ToolKind::Other`].
    pub fn kind(self) -> crate::acp::ToolKind {
        match self {
            Self::Acp(tool) => tool.kind(),
            Self::Local => crate::acp::ToolKind::Other,
        }
    }
}

struct ToolRegistryEntry {
    tool: SupportedTool,
    definition: ToolDefinition,
}

pub struct AcpToolRegistry {
    entries: Vec<ToolRegistryEntry>,
    local_definitions: Vec<ToolDefinition>,
    mapping: HashMap<String, ToolDescriptor>,
}

impl AcpToolRegistry {
    /// Build a registry from the enabled ACP tools plus any locally registered
    /// definitions.
    pub(crate) fn new(
        workspace_root: &Path,
        read_file_enabled: bool,
        list_files_enabled: bool,
        local_definitions: Vec<ToolDefinition>,
    ) -> Self {
        let mut entries = Vec::with_capacity(5);
        let mut mapping = HashMap::with_capacity(10);

        if read_file_enabled {
            push_registry_entry(
                &mut entries,
                &mut mapping,
                SupportedTool::ReadFile,
                build_read_file_definition(workspace_root),
            );
        }
        if list_files_enabled {
            push_registry_entry(
                &mut entries,
                &mut mapping,
                SupportedTool::ListFiles,
                build_list_files_definition(workspace_root),
            );
        }

        entries.sort_unstable_by_key(|entry| entry.tool.sort_key());

        Self { entries, local_definitions, mapping }
    }

    pub(crate) fn registered_tools(&self) -> Vec<SupportedTool> {
        self.entries.iter().map(|entry| entry.tool).collect()
    }

    pub(crate) fn definitions_for(&self, enabled_tools: &[SupportedTool], include_local: bool) -> Vec<ToolDefinition> {
        self.definitions_for_filtered(enabled_tools, include_local, |_| true)
    }

    /// Like [`Self::definitions_for`], but filters the local tool definitions
    /// through `local_tool_allowed`. This lets callers apply per-agent
    /// permission gating to local tools while keeping the ACP-advertised tools
    /// (already gated via `enabled_tools`) unchanged.
    pub(crate) fn definitions_for_filtered(
        &self,
        enabled_tools: &[SupportedTool],
        include_local: bool,
        local_tool_allowed: impl Fn(&str) -> bool,
    ) -> Vec<ToolDefinition> {
        let mut definitions = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            if enabled_tools.contains(&entry.tool) {
                definitions.push(entry.definition.clone());
            }
        }

        if include_local {
            definitions.extend(
                self.local_definitions
                    .iter()
                    .filter(|definition| local_tool_allowed(definition.function_name()))
                    .cloned(),
            );
        }

        definitions
    }

    pub(crate) fn render_title(&self, descriptor: ToolDescriptor, function_name: &str, args: &Value) -> String {
        render_title(descriptor, function_name, args)
    }

    pub fn tool_kind(&self, function_name: &str) -> crate::acp::ToolKind {
        self.tool_kind_for_call(function_name, None)
    }

    /// ACP tool kind for a call; `args` is accepted for future call-specific
    /// classification and currently ignored.
    pub(crate) fn tool_kind_for_call(&self, function_name: &str, args: Option<&Value>) -> crate::acp::ToolKind {
        let _ = args;
        match function_name {
            tools::READ_FILE => crate::acp::ToolKind::Read,
            tools::GREP_FILE | tools::LIST_FILES | tools::CODE_SEARCH => crate::acp::ToolKind::Search,
            tools::RUN_PTY_CMD
            | tools::EXEC_PTY_CMD
            | tools::EXEC_COMMAND
            | tools::WRITE_STDIN
            | tools::EXECUTE_CODE
            | tools::SHELL => crate::acp::ToolKind::Execute,
            tools::WRITE_FILE
            | tools::CREATE_FILE
            | tools::EDIT_FILE
            | tools::APPLY_PATCH
            | tools::SEARCH_REPLACE
            | tools::FILE_OP
            | tools::COPY_FILE => crate::acp::ToolKind::Edit,
            tools::DELETE_FILE => crate::acp::ToolKind::Delete,
            tools::MOVE_FILE => crate::acp::ToolKind::Move,
            tools::WEB_FETCH | tools::FETCH_URL | tools::FETCH => crate::acp::ToolKind::Fetch,
            tools::THINK => crate::acp::ToolKind::Think,
            _ => crate::acp::ToolKind::Other,
        }
    }

    /// Resolve a function name to its descriptor, preferring advertised ACP
    /// tools and falling back to the local definitions.
    pub(crate) fn lookup(&self, function_name: &str) -> Option<ToolDescriptor> {
        self.mapping.get(function_name).copied().or_else(|| {
            self.local_definitions
                .iter()
                .any(|definition| definition.function_name() == function_name)
                .then_some(ToolDescriptor::Local)
        })
    }

    pub(crate) fn has_local_tools(&self) -> bool {
        !self.local_definitions.is_empty()
    }
}

fn push_registry_entry(
    entries: &mut Vec<ToolRegistryEntry>,
    mapping: &mut HashMap<String, ToolDescriptor>,
    tool: SupportedTool,
    definition: ToolDefinition,
) {
    let _previous = mapping.insert(definition.function_name().to_string(), ToolDescriptor::Acp(tool));
    entries.push(ToolRegistryEntry { tool, definition });
}

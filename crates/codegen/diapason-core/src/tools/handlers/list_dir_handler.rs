//! List directory handler (from Codex pattern).
//!
//! Lists directory contents with metadata.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::sandboxing::{Sandboxable, SandboxablePreference};
use super::tool_handler::{ToolCallError, ToolHandler, ToolInvocation, ToolKind, ToolOutput, ToolPayload};

use crate::utils::formatting::format_size;

/// Maximum entries to return.
const MAX_ENTRIES: usize = 500;

/// Arguments for list_dir tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ListDirArgs {
    /// Path to list (defaults to current directory).
    pub path: Option<String>,
    /// Include hidden files.
    #[serde(default)]
    pub show_hidden: bool,
    /// Recursive listing.
    #[serde(default)]
    pub recursive: bool,
    /// Maximum depth for recursive listing.
    pub max_depth: Option<usize>,
    /// Pattern to filter entries (glob).
    pub pattern: Option<String>,
}

/// A directory entry.
#[derive(Clone, Debug, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub modified: Option<String>,
}

/// Handler for listing directories.
pub struct ListDirHandler {
    pub max_entries: usize,
}

impl Default for ListDirHandler {
    fn default() -> Self {
        Self { max_entries: MAX_ENTRIES }
    }
}

impl ListDirHandler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse arguments from payload.
    fn parse_args(&self, invocation: &ToolInvocation) -> Result<ListDirArgs, ToolCallError> {
        match &invocation.payload {
            ToolPayload::Function { arguments } => serde_json::from_str(arguments)
                .map_err(|e| ToolCallError::respond(format!("Invalid list_dir arguments: {e}"))),
            _ => Err(ToolCallError::respond("Invalid payload type for list_dir handler")),
        }
    }

    /// List directory contents.
    async fn list_directory(
        &self,
        path: &PathBuf,
        args: &ListDirArgs,
        depth: usize,
    ) -> Result<Vec<DirEntry>, ToolCallError> {
        let metadata = match tokio::fs::metadata(path).await {
            Ok(metadata) => metadata,
            Err(_) => return Err(ToolCallError::respond(format!("Directory not found: {}", path.display()))),
        };
        if !metadata.is_dir() {
            return Err(ToolCallError::respond(format!("Not a directory: {}", path.display())));
        }

        let max_depth = args.max_depth.unwrap_or(3);
        if args.recursive && depth >= max_depth {
            return Ok(Vec::new());
        }
        let pattern = args.pattern.as_deref().and_then(|pattern| glob::Pattern::new(pattern).ok());

        let mut entries = Vec::new();
        let mut dir_entries = tokio::fs::read_dir(path)
            .await
            .map_err(|e| ToolCallError::respond(format!("Cannot read directory: {e}")))?;

        while let Ok(Some(entry)) = dir_entries.next_entry().await {
            if entries.len() >= self.max_entries {
                break;
            }

            let entry_path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files unless requested
            if !args.show_hidden && file_name.starts_with('.') {
                continue;
            }

            // Apply pattern filter
            if let Some(pattern) = &pattern
                && !pattern.matches(&file_name)
            {
                continue;
            }

            let metadata = entry.metadata().await.ok();
            let is_dir = metadata.as_ref().is_some_and(std::fs::Metadata::is_dir);

            let dir_entry = DirEntry {
                name: file_name.clone(),
                path: entry_path.to_string_lossy().to_string(),
                is_dir,
                size: metadata.as_ref().map(|m| m.len()),
                modified: metadata.as_ref().and_then(|m| {
                    m.modified()
                        .ok()
                        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).format("%Y-%m-%d %H:%M:%S").to_string())
                }),
            };

            entries.push(dir_entry);

            // Recurse into subdirectories
            if args.recursive && is_dir && entries.len() < self.max_entries {
                let sub_entries = Box::pin(self.list_directory(&entry_path, args, depth + 1)).await?;
                entries.extend(sub_entries);
            }
        }

        // Sort entries: directories first, then alphabetically
        entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        });

        Ok(entries)
    }
}

impl Sandboxable for ListDirHandler {
    fn sandbox_preference(&self) -> SandboxablePreference {
        SandboxablePreference::Auto
    }

    fn escalate_on_failure(&self) -> bool {
        false
    }
}

#[async_trait]
impl ToolHandler for ListDirHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        false
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolCallError> {
        let args = self.parse_args(&invocation)?;
        let path = invocation.turn.resolve_path_ref(args.path.as_deref());

        let entries = self.list_directory(&path, &args, 0).await?;

        if entries.is_empty() {
            return Ok(ToolOutput::simple("Directory is empty."));
        }

        // Format as tree-like output. `write!` into `output` directly avoids
        // the per-entry temporary `String` that `format!` + `push_str` creates.
        use std::fmt::Write as _;
        let mut output = String::with_capacity(entries.len() * 40);
        for entry in &entries {
            let prefix = if entry.is_dir { "📁 " } else { "📄 " };
            let suffix = if entry.is_dir { "/" } else { "" };

            // Size is only shown for files (not directories); the previous
            // three-branch structure had two identical fall-through branches.
            if let Some(size) = entry.size
                && !entry.is_dir
            {
                let _ = writeln!(output, "{}{}{} ({})", prefix, entry.name, suffix, format_size(size));
            } else {
                let _ = writeln!(output, "{}{}{}", prefix, entry.name, suffix);
            }
        }

        Ok(ToolOutput::simple(output.trim()))
    }
}

/// Create the list_dir tool specification.
pub fn create_list_dir_tool() -> super::tool_handler::ToolSpec {
    use super::tool_handler::{ResponsesApiTool, ToolSpec};

    ToolSpec::Function(ResponsesApiTool {
        name: "list_dir".to_string(),
        description:
            "List the contents of a directory. Returns file and directory names with metadata. To search code, use code_search with query and optional path, file_types, result_types, or max_results filters."
                .to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to list (defaults to current directory)"
                },
                "show_hidden": {
                    "type": "boolean",
                    "description": "Include hidden files (default: false)"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "List recursively (default: false)"
                },
                "max_depth": {
                    "type": "number",
                    "description": "Maximum depth for recursive listing (default: 3)"
                },
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to filter entries"
                }
            },
            "additionalProperties": false
        }),
        strict: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn list_dir_handler_disables_sandbox_retry() {
        assert!(!ListDirHandler::new().escalate_on_failure());
    }

    #[test]
    fn test_list_dir_handler_kind() {
        let handler = ListDirHandler::new();
        assert_eq!(handler.kind(), ToolKind::Function);
    }

    #[test]
    fn test_create_list_dir_tool_spec() {
        let spec = create_list_dir_tool();
        assert_eq!(spec.name(), "list_dir");
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500B");
        assert_eq!(format_size(1024), "1.0KB");
        assert_eq!(format_size(1536), "1.5KB");
        assert_eq!(format_size(1048576), "1.0MB");
        assert_eq!(format_size(1073741824), "1.0GB");
    }

    #[tokio::test]
    async fn list_directory_filters_entries_with_async_metadata() {
        let temp_dir = TempDir::new().expect("workspace tempdir");
        fs::create_dir(temp_dir.path().join("nested")).expect("nested directory");
        fs::write(temp_dir.path().join("main.rs"), "fn main() {}\n").expect("rust file");
        fs::write(temp_dir.path().join("notes.md"), "notes\n").expect("markdown file");

        let args = ListDirArgs {
            path: None,
            show_hidden: false,
            recursive: false,
            max_depth: None,
            pattern: Some("*.rs".to_string()),
        };
        let entries = ListDirHandler::new()
            .list_directory(&temp_dir.path().to_path_buf(), &args, 0)
            .await
            .expect("directory listing should succeed");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "main.rs");
        assert!(!entries[0].is_dir);
        assert!(entries[0].size.is_some());
    }
}

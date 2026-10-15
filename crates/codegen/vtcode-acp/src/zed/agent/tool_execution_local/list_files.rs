//! Workspace-local file listing and ACP result projection.
use super::super::ZedAgent;
use crate::acp;
use crate::reports::{TOOL_RESPONSE_KEY_STATUS, TOOL_RESPONSE_KEY_TOOL, TOOL_SUCCESS_LABEL, ToolExecutionReport};
use crate::tooling::{
    TOOL_LIST_FILES_ITEMS_KEY, TOOL_LIST_FILES_MESSAGE_KEY, TOOL_LIST_FILES_PATH_ARG, TOOL_LIST_FILES_RESULT_KEY,
    TOOL_LIST_FILES_SUMMARY_MAX_ITEMS, TOOL_LIST_FILES_URI_ARG,
};
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;
use tracing::warn;
use vtcode_core::config::constants::tools;
use vtcode_core::tools::file_ops::FileOpsTool;
use vtcode_core::tools::traits::Tool;
use vtcode_core::utils::path::ensure_path_within_workspace;

use crate::zed::types::SessionHandle;

impl ZedAgent {
    pub(super) async fn run_list_files_inner(&self, args: &Value) -> Result<ToolExecutionReport, String> {
        self.run_list_files_for_tool(self.file_ops_tool.as_ref(), None, args).await
    }

    pub(super) async fn run_session_list_files_inner(
        &self,
        session: &SessionHandle,
        args: &Value,
    ) -> Result<ToolExecutionReport, String> {
        let runtime = match session.workspace_runtime() {
            Ok(Some(runtime)) => runtime,
            Ok(None) => return self.run_list_files_inner(args).await,
            Err(error) => return Err(format!("Failed to read ACP session workspace runtime: {error:#}")),
        };
        self.run_list_files_for_tool(runtime.file_ops_tool.as_ref(), Some(session), args)
            .await
    }

    async fn run_list_files_for_tool(
        &self,
        tool: Option<&FileOpsTool>,
        session: Option<&SessionHandle>,
        args: &Value,
    ) -> Result<ToolExecutionReport, String> {
        let Some(tool) = tool else {
            return Err("List files tool is unavailable".to_string());
        };

        let resolved_path = self.resolve_list_files_path(session, args)?.unwrap_or_else(|| ".".into());

        let mut normalized_args = match args.clone() {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        drop(normalized_args.insert(TOOL_LIST_FILES_PATH_ARG.to_string(), Value::String(resolved_path)));
        let normalized_args = Value::Object(normalized_args);

        let listing = tool.execute(normalized_args).await.map_err(|error| {
            let detail = error.to_string();
            warn!(error = %detail, "Failed to execute list_files tool");
            format!("Unable to list files: {detail}")
        })?;

        let content = Self::list_files_content(&listing);
        let locations = Self::list_files_locations(&listing);
        let payload = json!({
            TOOL_RESPONSE_KEY_STATUS: TOOL_SUCCESS_LABEL,
            TOOL_RESPONSE_KEY_TOOL: tools::LIST_FILES,
            TOOL_LIST_FILES_RESULT_KEY: listing,
        });

        Ok(ToolExecutionReport::success(content, locations, payload))
    }

    fn resolve_list_files_path(&self, session: Option<&SessionHandle>, args: &Value) -> Result<Option<String>, String> {
        if let Some(path) = args
            .get(TOOL_LIST_FILES_PATH_ARG)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            return Ok(Some(path.to_string()));
        }

        if let Some(uri) = args
            .get(TOOL_LIST_FILES_URI_ARG)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            let resolved = if let Some(session) = session {
                self.parse_resource_path(session, uri)?
            } else {
                let candidate = Self::decode_resource_uri(uri)?;
                self.resolve_workspace_path(candidate, TOOL_LIST_FILES_URI_ARG)?
            };
            let workspace_runtime = match session {
                Some(session) => session
                    .workspace_runtime()
                    .map_err(|error| format!("Failed to read ACP session workspace runtime: {error:#}"))?,
                None => None,
            };
            let workspace_root = workspace_runtime
                .as_ref()
                .map_or_else(|| self.workspace_root(), |runtime| runtime.workspace_root.as_path());
            let normalized = ensure_path_within_workspace(&resolved, workspace_root)
                .map_err(|_err| "list_files path must stay within the workspace".to_string())?;

            if normalized == workspace_root {
                return Ok(Some(".".into()));
            }

            if let Ok(relative) = normalized.strip_prefix(workspace_root) {
                if relative.as_os_str().is_empty() {
                    return Ok(Some(".".into()));
                }
                return Ok(Some(relative.to_string_lossy().into()));
            }

            return Ok(Some(normalized.to_string_lossy().into()));
        }

        Ok(None)
    }

    fn list_files_content(output: &Value) -> Vec<acp::ToolCallContent> {
        let mut lines = Vec::new();

        if let (Some(count), Some(total)) =
            (output.get("count").and_then(Value::as_u64), output.get("total").and_then(Value::as_u64))
        {
            lines.push(format!("Showing {count} of {total} items"));
        }

        if let Some(items) = output.get(TOOL_LIST_FILES_ITEMS_KEY).and_then(Value::as_array) {
            if items.is_empty() {
                lines.push("No items found.".to_string());
            } else {
                for item in items.iter().take(TOOL_LIST_FILES_SUMMARY_MAX_ITEMS) {
                    let path = item
                        .get("path")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("name").and_then(Value::as_str))
                        .unwrap_or("(unknown)");
                    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("file");
                    let prefix = match item_type {
                        "directory" => "[dir]",
                        "file" => "[file]",
                        other => other,
                    };
                    lines.push(format!("{prefix} {path}"));
                }

                if items.len() > TOOL_LIST_FILES_SUMMARY_MAX_ITEMS {
                    let remaining = items.len() - TOOL_LIST_FILES_SUMMARY_MAX_ITEMS;
                    lines.push(format!("… and {remaining} more"));
                }
            }
        } else {
            lines.push("No results returned.".to_string());
        }

        if let Some(has_more) = output.get("has_more").and_then(Value::as_bool)
            && has_more
        {
            lines.push("Additional results available (adjust page or per_page to view more).".to_string());
        }

        if let Some(message) = output
            .get(TOOL_LIST_FILES_MESSAGE_KEY)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            lines.push(message.to_string());
        }

        if lines.is_empty() {
            lines.push("No results.".to_string());
        }

        vec![acp::ToolCallContent::from(lines.join(" "))]
    }

    fn list_files_locations(output: &Value) -> Vec<acp::ToolCallLocation> {
        let Some(items) = output.get(TOOL_LIST_FILES_ITEMS_KEY).and_then(Value::as_array) else {
            return Vec::new();
        };

        items
            .iter()
            .filter_map(|item| {
                item.get("path")
                    .or_else(|| item.get("name"))
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
            })
            .take(TOOL_LIST_FILES_SUMMARY_MAX_ITEMS)
            .map(acp::ToolCallLocation::new)
            .collect()
    }
}

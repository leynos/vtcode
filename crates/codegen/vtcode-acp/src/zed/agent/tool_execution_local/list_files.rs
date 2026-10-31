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
use vtcode_core::config::constants::tools;
use vtcode_core::tools::file_ops::FileOpsTool;
use vtcode_core::tools::traits::Tool;
use vtcode_core::utils::path::ensure_path_within_workspace;

use crate::zed::agent::log_suppressed_error;
use crate::zed::constants::{
    SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE, TOOL_LIST_FILES_FAILURE, TOOL_LIST_FILES_UNAVAILABLE_FAILURE,
};
use crate::zed::types::SessionHandle;

/// Operation label for every log event emitted by this ACP file-listing boundary.
const LIST_FILES_OPERATION: &str = "list_files";

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
            Err(error) => return Err(Self::report_list_files_runtime_failure(&error)),
        };
        self.run_list_files_for_tool(runtime.file_ops_tool.as_ref(), Some(session), args)
            .await
    }

    pub(in crate::zed::agent) async fn run_list_files_for_tool(
        &self,
        tool: Option<&FileOpsTool>,
        session: Option<&SessionHandle>,
        args: &Value,
    ) -> Result<ToolExecutionReport, String> {
        let Some(tool) = tool else {
            tracing::warn!(
                operation = LIST_FILES_OPERATION,
                component = "list_files",
                outcome = "failed",
                error_category = "tool_unavailable",
                "ACP list_files tool is unavailable for this session"
            );
            return Err(TOOL_LIST_FILES_UNAVAILABLE_FAILURE.to_string());
        };

        let resolved_path = self.resolve_list_files_path(session, args)?.unwrap_or_else(|| ".".into());

        let mut normalized_args = match args.clone() {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        drop(normalized_args.insert(TOOL_LIST_FILES_PATH_ARG.to_string(), Value::String(resolved_path)));
        let normalized_args = Value::Object(normalized_args);

        let listing = tool
            .execute(normalized_args)
            .await
            .map_err(|error| Self::report_list_files_tool_failure(&error))?;

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
                    .map_err(|error| Self::report_list_files_runtime_failure(&error))?,
                None => None,
            };
            let workspace_root = workspace_runtime
                .as_ref()
                .map_or_else(|| self.workspace_root(), |runtime| runtime.workspace_root.as_path());
            let normalized = ensure_path_within_workspace(&resolved, workspace_root)
                .map_err(|_err| Self::report_list_files_path_escape())?;

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

    /// Record why the file-listing tool failed and return a fixed client-facing
    /// failure.
    ///
    /// The tool's `anyhow` chain can embed workspace paths, requested modes and
    /// other caller-supplied text, so only its depth is published at warning
    /// level while the full chain stays at debug.
    pub(super) fn report_list_files_tool_failure(error: &anyhow::Error) -> String {
        log_suppressed_error(error, LIST_FILES_OPERATION, "listing_failed");
        tracing::warn!(
            operation = LIST_FILES_OPERATION,
            component = "list_files",
            outcome = "failed",
            error_category = "listing_failed",
            error_chain_depth = error.chain().count(),
            "ACP list_files tool failed to list the workspace"
        );
        TOOL_LIST_FILES_FAILURE.to_string()
    }

    /// Record why the session workspace runtime could not be read and return a
    /// fixed client-facing failure.
    ///
    /// The underlying error is an `anyhow` chain whose context names the runtime
    /// lock and whose sources may carry workspace paths, so only its depth is
    /// published at warning level while the full chain stays at debug.
    pub(super) fn report_list_files_runtime_failure(error: &anyhow::Error) -> String {
        log_suppressed_error(error, LIST_FILES_OPERATION, "workspace_runtime_unavailable");
        tracing::warn!(
            operation = LIST_FILES_OPERATION,
            component = "workspace_runtime",
            outcome = "failed",
            error_category = "workspace_runtime_unavailable",
            error_chain_depth = error.chain().count(),
            "ACP list_files could not read the session workspace runtime"
        );
        SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE.to_string()
    }

    /// Record a workspace-escape rejection without echoing the requested path.
    pub(super) fn report_list_files_path_escape() -> String {
        tracing::warn!(
            operation = LIST_FILES_OPERATION,
            component = "path_resolution",
            outcome = "rejected",
            error_category = "workspace_path_escape",
            "ACP list_files rejected a path outside the trusted workspace"
        );
        "Invalid list_files uri value: path escapes the trusted workspace".to_string()
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

#[cfg(test)]
mod tests {
    use super::ZedAgent;
    use crate::zed::agent::test_log_capture::{capturing, capturing_at};
    use crate::zed::constants::{
        SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE, TOOL_LIST_FILES_FAILURE, TOOL_LIST_FILES_UNAVAILABLE_FAILURE,
    };

    #[test]
    fn runtime_failure_reports_a_fixed_reason_without_workspace_detail() {
        let private_path = "/home/private/workspace/session-runtime.lock";
        let error = anyhow::anyhow!("lock poisoned while reading {private_path}")
            .context("reading the ACP session workspace runtime");

        let (message, events) = capturing(|| ZedAgent::report_list_files_runtime_failure(&error));

        assert_eq!(
            message, SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE,
            "the client-facing failure must be a fixed message"
        );
        assert!(!message.contains(private_path), "the failure must not echo workspace detail");

        let captured = events.join("\n");
        assert!(!captured.contains(private_path), "workspace paths must stay out of logs");
        assert!(!captured.contains("lock poisoned"), "raw error text must stay out of logs");
        assert!(captured.contains("operation=\"list_files\""));
        assert!(captured.contains("component=\"workspace_runtime\""));
        assert!(captured.contains("outcome=\"failed\""));
        assert!(captured.contains("error_category=\"workspace_runtime_unavailable\""));
        assert!(captured.contains("error_chain_depth=2"));
    }

    #[test]
    fn runtime_failure_keeps_the_full_chain_only_at_debug() {
        let private_path = "/home/private/workspace/session-runtime.lock";
        let error = anyhow::anyhow!("lock poisoned while reading {private_path}")
            .context("reading the ACP session workspace runtime");
        let debug = tracing_subscriber::filter::LevelFilter::DEBUG;

        let (_, events) = capturing_at(debug, || ZedAgent::report_list_files_runtime_failure(&error));

        let captured = events.join("\n");
        assert!(
            captured.contains("reading the ACP session workspace runtime"),
            "the debug event must retain the chain for local diagnosis, got {captured}"
        );
        // The chain is deliberately available at debug; the warning beside it
        // is what reaches normal operator output, and that stays path-free.
        assert!(captured.contains("operation=\"list_files\""));
    }

    #[test]
    fn path_escape_rejection_does_not_echo_the_requested_path() {
        // The helper takes no path argument precisely so the rejected path can
        // never leak; assert the message is path-free as well as fixed.
        let (message, events) = capturing(ZedAgent::report_list_files_path_escape);

        assert_eq!(
            message, "Invalid list_files uri value: path escapes the trusted workspace",
            "the rejection must be a fixed, actionable message"
        );

        let captured = events.join("\n");
        for leaked in ["/home", "/tmp", "secret", ".."] {
            assert!(!message.contains(leaked), "the rejection must not embed {leaked:?}");
            assert!(!captured.contains(leaked), "rejected paths must stay out of logs");
        }
        assert!(captured.contains("component=\"path_resolution\""));
        assert!(captured.contains("outcome=\"rejected\""));
        assert!(captured.contains("error_category=\"workspace_path_escape\""));
    }

    #[test]
    fn client_facing_list_files_failures_carry_no_operational_detail() {
        for message in [
            TOOL_LIST_FILES_UNAVAILABLE_FAILURE,
            TOOL_LIST_FILES_FAILURE,
            SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE,
        ] {
            assert!(!message.is_empty());
            for leaked in ["/", "\\", "panic", "lock", "poison", "errno"] {
                assert!(!message.contains(leaked), "failure message must not embed {leaked:?}, got {message:?}");
            }
        }
    }
}

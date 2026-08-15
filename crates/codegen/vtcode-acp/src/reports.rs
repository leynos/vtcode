use crate::acp;
use serde_json::{Value, json};
use std::path::PathBuf;
use vtcode_safety::audit_log::ToolAuditStatus;

pub(crate) const TOOL_FAILURE_PREFIX: &str = "Tool execution failed";
pub(crate) const TOOL_SUCCESS_LABEL: &str = "success";
const TOOL_ERROR_LABEL: &str = "error";
pub(crate) const TOOL_RESPONSE_KEY_STATUS: &str = "status";
pub(crate) const TOOL_RESPONSE_KEY_TOOL: &str = "tool";
pub(crate) const TOOL_RESPONSE_KEY_PATH: &str = "path";
pub(crate) const TOOL_RESPONSE_KEY_CONTENT: &str = "content";
pub(crate) const TOOL_RESPONSE_KEY_CONTENT_HASH: &str = "content_hash";
pub(crate) const TOOL_RESPONSE_KEY_TRUNCATED: &str = "truncated";
const TOOL_RESPONSE_KEY_MESSAGE: &str = "message";

const TOOL_EXECUTION_CANCELLED_MESSAGE: &str = "Tool execution cancelled at the client's request";
pub const TOOL_PERMISSION_ALLOW_OPTION_ID: &str = "allow-once";
pub const TOOL_PERMISSION_ALLOW_ALWAYS_OPTION_ID: &str = "allow-always";
pub const TOOL_PERMISSION_DENY_OPTION_ID: &str = "reject-once";
pub const TOOL_PERMISSION_DENY_ALWAYS_OPTION_ID: &str = "reject-always";
pub(crate) const TOOL_PERMISSION_ALLOW_PREFIX: &str = "Allow";
pub(crate) const TOOL_PERMISSION_DENY_PREFIX: &str = "Deny";
pub const TOOL_PERMISSION_DENIED_MESSAGE: &str = "Tool execution cancelled: permission denied by the user";
pub const TOOL_PERMISSION_CANCELLED_MESSAGE: &str = "Tool execution cancelled: permission request interrupted";
pub(crate) const TOOL_PERMISSION_REQUEST_FAILURE_LOG: &str =
    "Failed to request ACP tool permission, cancelling the tool invocation";
pub(crate) const TOOL_PERMISSION_UNKNOWN_OPTION_LOG: &str = "Received unsupported ACP permission option selection";
pub const TOOL_PERMISSION_REQUEST_FAILURE_MESSAGE: &str = "Tool execution cancelled: permission request failed";

pub struct ToolExecutionReport {
    pub(crate) status: acp::ToolCallStatus,
    pub(crate) llm_response: String,
    pub(crate) content: Vec<acp::ToolCallContent>,
    pub(crate) locations: Vec<acp::ToolCallLocation>,
    pub(crate) raw_output: Option<Value>,
    pub(crate) audit_status: ToolAuditStatus,
}

impl ToolExecutionReport {
    pub(crate) fn success(
        content: Vec<acp::ToolCallContent>,
        locations: Vec<acp::ToolCallLocation>,
        payload: Value,
    ) -> Self {
        Self {
            status: acp::ToolCallStatus::Completed,
            llm_response: payload.to_string(),
            content,
            locations,
            raw_output: Some(payload),
            audit_status: ToolAuditStatus::Success,
        }
    }

    pub(crate) fn failure(tool_name: &str, message: &str) -> Self {
        let payload = json!({
            TOOL_RESPONSE_KEY_STATUS: TOOL_ERROR_LABEL,
            TOOL_RESPONSE_KEY_TOOL: tool_name,
            TOOL_RESPONSE_KEY_MESSAGE: message,
        });
        Self {
            status: acp::ToolCallStatus::Failed,
            llm_response: payload.to_string(),
            content: vec![acp::ToolCallContent::from(format!("{TOOL_FAILURE_PREFIX}: {message}"))],
            locations: Vec::new(),
            raw_output: Some(payload),
            audit_status: ToolAuditStatus::Failure,
        }
    }

    pub(crate) fn structured_failure(error: &vtcode_core::tools::registry::ToolExecutionError) -> Self {
        let payload = json!({
            TOOL_RESPONSE_KEY_STATUS: TOOL_ERROR_LABEL,
            TOOL_RESPONSE_KEY_TOOL: error.tool_name,
            TOOL_RESPONSE_KEY_MESSAGE: error.user_message(),
            "error": error,
        });
        Self {
            status: acp::ToolCallStatus::Failed,
            llm_response: payload.to_string(),
            content: vec![acp::ToolCallContent::from(format!(
                "{TOOL_FAILURE_PREFIX}: {}",
                error.user_message()
            ))],
            locations: Vec::new(),
            raw_output: Some(payload),
            audit_status: ToolAuditStatus::Failure,
        }
    }

    pub(crate) fn blocked(tool_name: &str, message: &str) -> Self {
        let mut report = Self::failure(tool_name, message);
        report.audit_status = ToolAuditStatus::Blocked;
        report
    }

    pub(crate) fn cancelled(tool_name: &str) -> Self {
        Self::cancelled_with_message(tool_name, TOOL_EXECUTION_CANCELLED_MESSAGE)
    }

    pub(crate) fn cancelled_with_message(tool_name: &str, message: &str) -> Self {
        let mut report = Self::failure(tool_name, message);
        report.audit_status = ToolAuditStatus::Cancelled;
        report
    }
}

pub(crate) fn create_diff_content(path: &str, old_text: Option<&str>, new_text: &str) -> acp::ToolCallContent {
    acp::ToolCallContent::Diff(
        acp::Diff::new(PathBuf::from(path), new_text.to_string()).old_text(old_text.map(|s| s.to_string())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtcode_safety::audit_log::ToolAuditStatus;

    #[test]
    fn maps_success_denied_and_failure_to_audit_statuses() {
        assert_eq!(
            ToolExecutionReport::success(Vec::new(), Vec::new(), json!({})).audit_status,
            ToolAuditStatus::Success
        );
        assert_eq!(ToolExecutionReport::blocked("read_file", "denied").audit_status, ToolAuditStatus::Blocked);
        assert_eq!(ToolExecutionReport::failure("read_file", "failed").audit_status, ToolAuditStatus::Failure);
        assert_eq!(ToolExecutionReport::cancelled("read_file").audit_status, ToolAuditStatus::Cancelled);
    }

    #[test]
    fn structured_failure_preserves_model_visible_error_details() {
        let error = vtcode_core::tools::registry::ToolExecutionError::new(
            "apply_patch",
            vtcode_core::tools::registry::ToolErrorType::InvalidParameters,
            "version mismatch",
        )
        .with_details(json!({"reason": "content_hash_mismatch"}));

        let report = ToolExecutionReport::structured_failure(&error);

        assert_eq!(report.raw_output.as_ref().expect("raw output")["error"]["details"], error.details.unwrap());
        assert!(report.llm_response.contains("content_hash_mismatch"));
    }
}

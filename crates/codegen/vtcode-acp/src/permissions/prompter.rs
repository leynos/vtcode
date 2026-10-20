//! Coordinates ACP client permission prompts and translates their responses into tool authorization decisions.

mod trait_impl;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::acp;
use crate::acp::Error as SdkError;
use crate::zed::connection::ConnectionHandle;
use async_trait::async_trait;
use serde_json::Value;
use tracing::{debug, error, warn};

use crate::reports::{
    TOOL_PERMISSION_ALLOW_ALWAYS_OPTION_ID, TOOL_PERMISSION_ALLOW_OPTION_ID, TOOL_PERMISSION_ALLOW_PREFIX,
    TOOL_PERMISSION_CANCELLED_MESSAGE, TOOL_PERMISSION_DENIED_MESSAGE, TOOL_PERMISSION_DENY_ALWAYS_OPTION_ID,
    TOOL_PERMISSION_DENY_OPTION_ID, TOOL_PERMISSION_DENY_PREFIX, TOOL_PERMISSION_REQUEST_FAILURE_LOG,
    TOOL_PERMISSION_REQUEST_FAILURE_MESSAGE, TOOL_PERMISSION_UNKNOWN_OPTION_LOG, ToolExecutionReport,
};

use crate::tooling::{SupportedTool, ToolDescriptor, ToolRegistryProvider};

/// Describes a tool request by its registered name, protocol kind, and prompt label.
#[derive(Clone, Copy, Debug)]
pub struct PermissionToolContext<'a> {
    name: &'a str,
    kind: acp::ToolKind,
    action_label: &'a str,
}

impl<'a> PermissionToolContext<'a> {
    #[must_use]
    pub(crate) fn new(name: &'a str, kind: acp::ToolKind, action_label: &'a str) -> Self {
        Self { name, kind, action_label }
    }
}

/// Prompts the connected ACP client for permission before running a tool call.
///
/// The trait stays object-safe (`Send + Sync`) so the agent can keep the
/// prompter behind an `Arc<dyn ...>` in its shared state and ship it into
/// SACP `cx.spawn` tasks along with the agent itself.
/// Permission-prompt operations used by ACP tool execution.
#[async_trait]
pub trait AcpPermissionPrompter: Send + Sync {
    /// Build the permission choices shown for a registered tool call.
    fn permission_options(&self, tool: SupportedTool, args: Option<&Value>) -> Vec<acp::PermissionOption>;

    /// Request client authorization before executing a registered tool call.
    async fn request_tool_permission(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: SupportedTool,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError>;

    /// Request permission even when normal confirmation bypasses are enabled.
    /// Lifecycle `PreToolUse=Ask` uses this explicit path so a quality gate
    /// cannot be silently defeated by `--dangerously-skip-permissions`.
    async fn request_tool_permission_forced(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: SupportedTool,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        self.request_tool_permission(client, session_id, call, tool, args).await
    }

    /// Request client authorization for a tool identified by its protocol name.
    async fn request_named_tool_permission(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: PermissionToolContext<'_>,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError>;

    /// Request named-tool authorization even when ordinary confirmations are skipped.
    async fn request_named_tool_permission_forced(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: PermissionToolContext<'_>,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        self.request_named_tool_permission(client, session_id, call, tool, args).await
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct PermissionDecisionKey {
    session_id: acp::SessionId,
    tool_name: String,
    action_label: String,
    args_json: String,
}

impl PermissionDecisionKey {
    fn for_request(request: NamedToolPermissionRequest<'_>) -> Self {
        Self {
            session_id: request.session_id.clone(),
            tool_name: request.tool.name.to_string(),
            action_label: request.tool.action_label.to_string(),
            args_json: request.args.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PersistentPermissionDecision {
    Allow,
    Reject,
}

#[derive(Clone, Copy)]
enum PermissionPromptMode {
    Standard,
    Forced,
}

#[derive(Clone, Copy)]
struct NamedToolPermissionRequest<'a> {
    session_id: &'a acp::SessionId,
    call: &'a acp::ToolCall,
    tool: PermissionToolContext<'a>,
    args: &'a Value,
    mode: PermissionPromptMode,
}

#[derive(Clone, Copy)]
struct SupportedToolPermissionRequest<'a> {
    session_id: &'a acp::SessionId,
    call: &'a acp::ToolCall,
    tool: SupportedTool,
    args: &'a Value,
    mode: PermissionPromptMode,
}

#[derive(Clone, Copy)]
enum PermissionRequestDisposition {
    Bypassed,
    Reuse(PersistentPermissionDecision),
    Prompt,
}

/// Requests tool permission from the connected ACP client and caches session decisions.
pub struct DefaultPermissionPrompter<P> {
    registry: P,
    skip_confirmations: bool,
    persistent_decisions: Mutex<HashMap<PermissionDecisionKey, PersistentPermissionDecision>>,
}

impl<P> DefaultPermissionPrompter<P>
where
    P: ToolRegistryProvider,
{
    /// Create a prompter that requests confirmation for each tool call unless a
    /// persistent decision has already been recorded for that request.
    pub fn new(registry: P) -> Self {
        Self::with_skip_confirmations(registry, false)
    }

    /// Builds a prompter that can bypass ordinary permission confirmations.
    ///
    /// When `skip_confirmations` is `true`, standard permission requests are
    /// bypassed. Forced permission requests still prompt the client.
    pub fn with_skip_confirmations(registry: P, skip_confirmations: bool) -> Self {
        Self {
            registry,
            skip_confirmations,
            persistent_decisions: Mutex::new(HashMap::new()),
        }
    }

    fn persistent_decision(&self, key: &PermissionDecisionKey) -> Option<PersistentPermissionDecision> {
        self.persistent_decisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .copied()
    }

    fn remember_persistent_decision(&self, key: PermissionDecisionKey, decision: PersistentPermissionDecision) {
        let _previous = self
            .persistent_decisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, decision);
    }

    fn render_action_label(&self, tool: SupportedTool, args: Option<&Value>) -> String {
        if let Some(arguments) = args {
            self.registry
                .render_title(ToolDescriptor::Acp(tool), tool.function_name(), arguments)
        } else {
            tool.default_title().to_string()
        }
    }

    fn permission_options_for_action(&self, action_label: &str) -> Vec<acp::PermissionOption> {
        let allow_once_option = acp::PermissionOption::new(
            acp::PermissionOptionId::from(Arc::from(TOOL_PERMISSION_ALLOW_OPTION_ID)),
            format!("{TOOL_PERMISSION_ALLOW_PREFIX} {action_label} once"),
            acp::PermissionOptionKind::AllowOnce,
        );

        let allow_always_option = acp::PermissionOption::new(
            acp::PermissionOptionId::from(Arc::from(TOOL_PERMISSION_ALLOW_ALWAYS_OPTION_ID)),
            format!("{TOOL_PERMISSION_ALLOW_PREFIX} {action_label} always"),
            acp::PermissionOptionKind::AllowAlways,
        );

        let deny_once_option = acp::PermissionOption::new(
            acp::PermissionOptionId::from(Arc::from(TOOL_PERMISSION_DENY_OPTION_ID)),
            format!("{TOOL_PERMISSION_DENY_PREFIX} {action_label} once"),
            acp::PermissionOptionKind::RejectOnce,
        );

        let deny_always_option = acp::PermissionOption::new(
            acp::PermissionOptionId::from(Arc::from(TOOL_PERMISSION_DENY_ALWAYS_OPTION_ID)),
            format!("{TOOL_PERMISSION_DENY_PREFIX} {action_label} always"),
            acp::PermissionOptionKind::RejectAlways,
        );

        vec![
            allow_once_option,
            allow_always_option,
            deny_once_option,
            deny_always_option,
        ]
    }
}

impl<P> DefaultPermissionPrompter<P>
where
    P: ToolRegistryProvider + Send + Sync,
{
    async fn request_supported_tool_permission(
        &self,
        client: &ConnectionHandle,
        request: SupportedToolPermissionRequest<'_>,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        let action_label = self.render_action_label(request.tool, Some(request.args));
        self.request_named_tool_permission_impl(
            client,
            NamedToolPermissionRequest {
                session_id: request.session_id,
                call: request.call,
                tool: PermissionToolContext::new(request.tool.function_name(), request.tool.kind(), &action_label),
                args: request.args,
                mode: request.mode,
            },
        )
        .await
    }

    fn request_disposition(&self, request: NamedToolPermissionRequest<'_>) -> PermissionRequestDisposition {
        if matches!(request.mode, PermissionPromptMode::Standard) {
            if self.skip_confirmations {
                return PermissionRequestDisposition::Bypassed;
            }

            if let Some(decision) = self.persistent_decision(&PermissionDecisionKey::for_request(request)) {
                return PermissionRequestDisposition::Reuse(decision);
            }
        }

        PermissionRequestDisposition::Prompt
    }

    fn reused_decision_report(
        &self,
        request: NamedToolPermissionRequest<'_>,
        decision: PersistentPermissionDecision,
    ) -> Option<ToolExecutionReport> {
        match decision {
            PersistentPermissionDecision::Allow => {
                debug!(
                    session_id = %request.session_id,
                    tool = request.tool.name,
                    "ACP permission allowed by session decision"
                );
                None
            }
            PersistentPermissionDecision::Reject => {
                debug!(
                    session_id = %request.session_id,
                    tool = request.tool.name,
                    "ACP permission rejected by session decision"
                );
                Some(ToolExecutionReport::blocked(request.tool.name, TOOL_PERMISSION_DENIED_MESSAGE))
            }
        }
    }

    async fn request_named_tool_permission_impl(
        &self,
        client: &ConnectionHandle,
        request: NamedToolPermissionRequest<'_>,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        match self.request_disposition(request) {
            PermissionRequestDisposition::Bypassed => {
                debug!(
                    session_id = %request.session_id,
                    tool = request.tool.name,
                    "ACP permission prompt bypassed"
                );
                Ok(None)
            }
            PermissionRequestDisposition::Reuse(decision) => Ok(self.reused_decision_report(request, decision)),
            PermissionRequestDisposition::Prompt => self.prompt_for_permission(client, request).await,
        }
    }

    async fn prompt_for_permission(
        &self,
        client: &ConnectionHandle,
        request: NamedToolPermissionRequest<'_>,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        let fields = acp::ToolCallUpdateFields::default()
            .title(request.call.title.clone())
            .kind(request.tool.kind)
            .status(acp::ToolCallStatus::Pending)
            .raw_input(request.args.clone());

        let acp_request = acp::RequestPermissionRequest::new(
            request.session_id.clone(),
            acp::ToolCallUpdate::new(request.call.tool_call_id.clone(), fields),
            self.permission_options_for_action(request.tool.action_label),
        );

        match client.request_permission(acp_request).await {
            Ok(response) => Ok(self.map_permission_response(request, response.outcome)),
            Err(error) => {
                error!(%error, "{}", TOOL_PERMISSION_REQUEST_FAILURE_LOG);
                Ok(Some(ToolExecutionReport::failure(request.tool.name, TOOL_PERMISSION_REQUEST_FAILURE_MESSAGE)))
            }
        }
    }

    fn map_permission_response(
        &self,
        request: NamedToolPermissionRequest<'_>,
        outcome: acp::RequestPermissionOutcome,
    ) -> Option<ToolExecutionReport> {
        match outcome {
            acp::RequestPermissionOutcome::Cancelled => {
                Some(ToolExecutionReport::cancelled_with_message(request.tool.name, TOOL_PERMISSION_CANCELLED_MESSAGE))
            }
            acp::RequestPermissionOutcome::Selected(outcome) => {
                self.map_permission_option(request, outcome.option_id.0.as_ref())
            }
            _ => Some(ToolExecutionReport::blocked(request.tool.name, TOOL_PERMISSION_DENIED_MESSAGE)),
        }
    }

    fn map_permission_option(
        &self,
        request: NamedToolPermissionRequest<'_>,
        option_id: &str,
    ) -> Option<ToolExecutionReport> {
        let decision_key = PermissionDecisionKey::for_request(request);
        match option_id {
            TOOL_PERMISSION_ALLOW_OPTION_ID => None,
            TOOL_PERMISSION_ALLOW_ALWAYS_OPTION_ID => {
                self.remember_persistent_decision(decision_key, PersistentPermissionDecision::Allow);
                None
            }
            TOOL_PERMISSION_DENY_OPTION_ID => {
                Some(ToolExecutionReport::blocked(request.tool.name, TOOL_PERMISSION_DENIED_MESSAGE))
            }
            TOOL_PERMISSION_DENY_ALWAYS_OPTION_ID => {
                self.remember_persistent_decision(decision_key, PersistentPermissionDecision::Reject);
                Some(ToolExecutionReport::blocked(request.tool.name, TOOL_PERMISSION_DENIED_MESSAGE))
            }
            _ => {
                warn!("{}", TOOL_PERMISSION_UNKNOWN_OPTION_LOG);
                Some(ToolExecutionReport::blocked(request.tool.name, TOOL_PERMISSION_DENIED_MESSAGE))
            }
        }
    }
}

//! Coordinates ACP client permission prompts and translates their responses into tool authorization decisions.
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

use super::tooling::{SupportedTool, ToolDescriptor, ToolRegistryProvider};

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
#[async_trait]
pub trait AcpPermissionPrompter: Send + Sync {
    fn permission_options(&self, tool: SupportedTool, args: Option<&Value>) -> Vec<acp::PermissionOption>;

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

    async fn request_named_tool_permission(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: PermissionToolContext<'_>,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError>;

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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PermissionDecisionKey {
    session_id: acp::SessionId,
    tool_name: String,
    action_label: String,
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

pub struct DefaultPermissionPrompter<P> {
    registry: P,
    skip_confirmations: bool,
    persistent_decisions: Mutex<HashMap<PermissionDecisionKey, PersistentPermissionDecision>>,
}

impl<P> DefaultPermissionPrompter<P>
where
    P: ToolRegistryProvider,
{
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

    fn permission_key(session_id: &acp::SessionId, tool_name: &str, action_label: &str) -> PermissionDecisionKey {
        PermissionDecisionKey {
            session_id: session_id.clone(),
            tool_name: tool_name.to_string(),
            action_label: action_label.to_string(),
        }
    }

    fn persistent_decision(
        &self,
        session_id: &acp::SessionId,
        tool_name: &str,
        action_label: &str,
    ) -> Option<PersistentPermissionDecision> {
        self.persistent_decisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&Self::permission_key(session_id, tool_name, action_label))
            .copied()
    }

    fn remember_persistent_decision(
        &self,
        session_id: &acp::SessionId,
        tool_name: &str,
        action_label: &str,
        decision: PersistentPermissionDecision,
    ) {
        let _previous = self
            .persistent_decisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(Self::permission_key(session_id, tool_name, action_label), decision);
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

#[async_trait]
impl<P> AcpPermissionPrompter for DefaultPermissionPrompter<P>
where
    P: ToolRegistryProvider + Send + Sync,
{
    fn permission_options(&self, tool: SupportedTool, args: Option<&Value>) -> Vec<acp::PermissionOption> {
        let action_label = self.render_action_label(tool, args);
        self.permission_options_for_action(&action_label)
    }

    async fn request_tool_permission(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: SupportedTool,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        self.request_supported_tool_permission(
            client,
            SupportedToolPermissionRequest {
                session_id,
                call,
                tool,
                args,
                mode: PermissionPromptMode::Standard,
            },
        )
        .await
    }

    async fn request_tool_permission_forced(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: SupportedTool,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        self.request_supported_tool_permission(
            client,
            SupportedToolPermissionRequest {
                session_id,
                call,
                tool,
                args,
                mode: PermissionPromptMode::Forced,
            },
        )
        .await
    }

    async fn request_named_tool_permission(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: PermissionToolContext<'_>,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        self.request_named_tool_permission_impl(
            client,
            NamedToolPermissionRequest {
                session_id,
                call,
                tool,
                args,
                mode: PermissionPromptMode::Standard,
            },
        )
        .await
    }

    async fn request_named_tool_permission_forced(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: PermissionToolContext<'_>,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        self.request_named_tool_permission_impl(
            client,
            NamedToolPermissionRequest {
                session_id,
                call,
                tool,
                args,
                mode: PermissionPromptMode::Forced,
            },
        )
        .await
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

            if let Some(decision) =
                self.persistent_decision(request.session_id, request.tool.name, request.tool.action_label)
            {
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
        match option_id {
            TOOL_PERMISSION_ALLOW_OPTION_ID => None,
            TOOL_PERMISSION_ALLOW_ALWAYS_OPTION_ID => {
                self.remember_persistent_decision(
                    request.session_id,
                    request.tool.name,
                    request.tool.action_label,
                    PersistentPermissionDecision::Allow,
                );
                None
            }
            TOOL_PERMISSION_DENY_OPTION_ID => {
                Some(ToolExecutionReport::blocked(request.tool.name, TOOL_PERMISSION_DENIED_MESSAGE))
            }
            TOOL_PERMISSION_DENY_ALWAYS_OPTION_ID => {
                self.remember_persistent_decision(
                    request.session_id,
                    request.tool.name,
                    request.tool.action_label,
                    PersistentPermissionDecision::Reject,
                );
                Some(ToolExecutionReport::blocked(request.tool.name, TOOL_PERMISSION_DENIED_MESSAGE))
            }
            _ => {
                warn!("{}", TOOL_PERMISSION_UNKNOWN_OPTION_LOG);
                Some(ToolExecutionReport::blocked(request.tool.name, TOOL_PERMISSION_DENIED_MESSAGE))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reports::{
        TOOL_PERMISSION_ALLOW_OPTION_ID, TOOL_PERMISSION_CANCELLED_MESSAGE, TOOL_PERMISSION_DENIED_MESSAGE,
        TOOL_PERMISSION_REQUEST_FAILURE_MESSAGE,
    };
    use crate::tooling::{AcpToolRegistry, SupportedTool};
    use crate::zed::connection::ConnectionHandle;
    use agent_client_protocol::schema::v1::{
        RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    };
    use agent_client_protocol::{Agent, Channel, Client, ConnectionTo, on_receive_request};
    use serde_json::json;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::oneshot;

    #[derive(Clone, Copy)]
    enum ClientDecision {
        Allow,
        AllowAlways,
        Deny,
        DenyAlways,
        Cancel,
        Unknown,
        RequestFailure,
    }

    fn test_prompter() -> DefaultPermissionPrompter<AcpToolRegistry> {
        DefaultPermissionPrompter::new(AcpToolRegistry::new(Path::new("/tmp"), true, true, Vec::new()))
    }

    async fn run_permission_flow(decision: ClientDecision) -> Option<ToolExecutionReport> {
        let (agent_channel, client_channel) = Channel::duplex();
        let (result_tx, result_rx) = oneshot::channel();
        let session_id = acp::SessionId::new("permission-test-session");
        let call = acp::ToolCall::new("permission-test-call", "Read file src/lib.rs");
        let args = json!({ "path": "src/lib.rs" });

        let agent = Agent
            .builder()
            .connect_with(agent_channel, async move |cx: ConnectionTo<Client>| {
                let handle = ConnectionHandle::new(cx);
                let result = test_prompter()
                    .request_tool_permission(&handle, &session_id, &call, SupportedTool::ReadFile, &args)
                    .await;
                drop(result_tx.send(result));
                Ok(())
            });

        let client = Client
            .builder()
            .on_receive_request(
                async move |request: RequestPermissionRequest, responder, _connection| {
                    assert_eq!(request.options.len(), 4);
                    let response = match decision {
                        ClientDecision::Allow => RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                            SelectedPermissionOutcome::new(TOOL_PERMISSION_ALLOW_OPTION_ID),
                        )),
                        ClientDecision::AllowAlways => {
                            RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                                SelectedPermissionOutcome::new(TOOL_PERMISSION_ALLOW_ALWAYS_OPTION_ID),
                            ))
                        }
                        ClientDecision::Deny => RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                            SelectedPermissionOutcome::new(TOOL_PERMISSION_DENY_OPTION_ID),
                        )),
                        ClientDecision::DenyAlways => {
                            RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                                SelectedPermissionOutcome::new(TOOL_PERMISSION_DENY_ALWAYS_OPTION_ID),
                            ))
                        }
                        ClientDecision::Cancel => RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
                        ClientDecision::Unknown => RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                            SelectedPermissionOutcome::new("unsupported-option"),
                        )),
                        ClientDecision::RequestFailure => {
                            return responder.respond_with_internal_error("simulated permission request failure");
                        }
                    };
                    responder.respond(response)
                },
                on_receive_request!(),
            )
            .connect_to(client_channel);

        let (agent_result, client_result) = tokio::join!(agent, client);
        agent_result.expect("agent duplex connection should complete");
        client_result.expect("client duplex connection should complete");
        result_rx
            .await
            .expect("agent should report the permission result")
            .expect("prompter should return a result")
    }

    async fn run_repeated_permission_flow(
        decision: ClientDecision,
        skip_confirmations: bool,
        force_prompt: bool,
        second_args: Option<Value>,
    ) -> (Vec<Option<ToolExecutionReport>>, usize) {
        let (agent_channel, client_channel) = Channel::duplex();
        let (result_tx, result_rx) = oneshot::channel();
        let request_count = Arc::new(AtomicUsize::new(0));
        let client_request_count = Arc::clone(&request_count);
        let session_id = acp::SessionId::new("persistent-permission-test-session");
        let call = acp::ToolCall::new("persistent-permission-test-call", "Read file src/lib.rs");
        let args = json!({ "path": "src/lib.rs" });

        let agent = Agent
            .builder()
            .connect_with(agent_channel, async move |cx: ConnectionTo<Client>| {
                let handle = ConnectionHandle::new(cx);
                let prompter = DefaultPermissionPrompter::with_skip_confirmations(
                    AcpToolRegistry::new(Path::new("/tmp"), true, true, Vec::new()),
                    skip_confirmations,
                );
                let mut results = Vec::with_capacity(2);
                for index in 0..2 {
                    let args = if index == 1 {
                        second_args.as_ref().unwrap_or(&args)
                    } else {
                        &args
                    };
                    results.push(if force_prompt {
                        prompter
                            .request_tool_permission_forced(&handle, &session_id, &call, SupportedTool::ReadFile, args)
                            .await
                    } else {
                        prompter
                            .request_tool_permission(&handle, &session_id, &call, SupportedTool::ReadFile, args)
                            .await
                    });
                }
                drop(result_tx.send(results));
                Ok(())
            });

        let client = Client
            .builder()
            .on_receive_request(
                async move |request: RequestPermissionRequest, responder, _connection| {
                    let _previous_count = client_request_count.fetch_add(1, Ordering::Relaxed);
                    assert_eq!(request.options.len(), 4);
                    let option_id = match decision {
                        ClientDecision::Allow => TOOL_PERMISSION_ALLOW_OPTION_ID,
                        ClientDecision::AllowAlways => TOOL_PERMISSION_ALLOW_ALWAYS_OPTION_ID,
                        ClientDecision::Deny => TOOL_PERMISSION_DENY_OPTION_ID,
                        ClientDecision::DenyAlways => TOOL_PERMISSION_DENY_ALWAYS_OPTION_ID,
                        ClientDecision::Cancel => {
                            return responder
                                .respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled));
                        }
                        ClientDecision::Unknown => "unsupported-option",
                        ClientDecision::RequestFailure => {
                            return responder.respond_with_internal_error("simulated permission request failure");
                        }
                    };
                    responder.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new(option_id),
                    )))
                },
                on_receive_request!(),
            )
            .connect_to(client_channel);

        let (agent_result, client_result) = tokio::join!(agent, client);
        agent_result.expect("agent duplex connection should complete");
        client_result.expect("client duplex connection should complete");
        let results = result_rx
            .await
            .expect("agent should report repeated permission results")
            .into_iter()
            .map(|result| result.expect("prompter should return a result"))
            .collect();
        (results, request_count.load(Ordering::Relaxed))
    }

    #[tokio::test]
    async fn permission_allow_flow_returns_no_failure() {
        assert!(run_permission_flow(ClientDecision::Allow).await.is_none());
    }

    #[tokio::test]
    async fn permission_deny_flow_returns_denied_report() {
        let report = run_permission_flow(ClientDecision::Deny)
            .await
            .expect("deny should produce a report");
        assert!(report.llm_response.contains(TOOL_PERMISSION_DENIED_MESSAGE));
    }

    #[tokio::test]
    async fn permission_cancel_flow_returns_cancelled_report() {
        let report = run_permission_flow(ClientDecision::Cancel)
            .await
            .expect("cancel should produce a report");
        assert!(report.llm_response.contains(TOOL_PERMISSION_CANCELLED_MESSAGE));
    }

    #[tokio::test]
    async fn permission_unknown_option_fails_closed() {
        let report = run_permission_flow(ClientDecision::Unknown)
            .await
            .expect("unknown option should be denied");
        assert!(report.llm_response.contains(TOOL_PERMISSION_DENIED_MESSAGE));
    }

    #[tokio::test]
    async fn permission_request_failure_returns_failure_report() {
        let report = run_permission_flow(ClientDecision::RequestFailure)
            .await
            .expect("request failure should produce a report");
        assert!(report.llm_response.contains(TOOL_PERMISSION_REQUEST_FAILURE_MESSAGE));
    }

    #[tokio::test]
    async fn skip_confirmations_bypasses_acp_permission_requests() {
        let (results, request_count) = run_repeated_permission_flow(ClientDecision::Allow, true, false, None).await;

        assert!(results.iter().all(Option::is_none));
        assert_eq!(request_count, 0);
    }

    #[tokio::test]
    async fn allow_always_is_reused_for_the_session_tool() {
        let (results, request_count) =
            run_repeated_permission_flow(ClientDecision::AllowAlways, false, false, None).await;

        assert!(results.iter().all(Option::is_none));
        assert_eq!(request_count, 1);
    }

    #[tokio::test]
    async fn reject_always_is_reused_for_the_session_tool() {
        let (results, request_count) =
            run_repeated_permission_flow(ClientDecision::DenyAlways, false, false, None).await;

        assert!(results.iter().all(|report| {
            report
                .as_ref()
                .is_some_and(|report| report.llm_response.contains(TOOL_PERMISSION_DENIED_MESSAGE))
        }));
        assert_eq!(request_count, 1);
    }

    #[tokio::test]
    async fn forced_permission_prompt_overrides_skip_confirmations() {
        let (results, request_count) = run_repeated_permission_flow(ClientDecision::Allow, true, true, None).await;

        assert!(results.iter().all(Option::is_none));
        assert_eq!(request_count, 2);
    }

    #[tokio::test]
    async fn forced_permission_prompt_overrides_cached_session_decisions() {
        let (results, request_count) =
            run_repeated_permission_flow(ClientDecision::AllowAlways, false, true, None).await;

        assert!(results.iter().all(Option::is_none));
        assert_eq!(request_count, 2);
    }

    #[tokio::test]
    async fn allow_always_does_not_cover_a_different_action_for_the_same_tool() {
        let (results, request_count) = run_repeated_permission_flow(
            ClientDecision::AllowAlways,
            false,
            false,
            Some(json!({ "path": "secrets.env" })),
        )
        .await;

        assert!(results.iter().all(Option::is_none));
        assert_eq!(request_count, 2);
    }
}

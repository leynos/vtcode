use super::*;
use crate::acp;
use crate::acp::Error as SdkError;
use crate::reports::{
    TOOL_PERMISSION_ALLOW_ALWAYS_OPTION_ID, TOOL_PERMISSION_ALLOW_OPTION_ID, TOOL_PERMISSION_CANCELLED_MESSAGE,
    TOOL_PERMISSION_DENIED_MESSAGE, TOOL_PERMISSION_DENY_ALWAYS_OPTION_ID, TOOL_PERMISSION_DENY_OPTION_ID,
    TOOL_PERMISSION_REQUEST_FAILURE_MESSAGE, ToolExecutionReport,
};
use crate::tooling::{AcpToolRegistry, SupportedTool};
use crate::zed::connection::ConnectionHandle;
use agent_client_protocol::schema::v1::{
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
};
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo, on_receive_request};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
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

enum PermissionClientAction {
    Select(&'static str),
    Cancel,
    Fail,
}

fn permission_client_action(decision: ClientDecision) -> PermissionClientAction {
    match decision {
        ClientDecision::Allow => PermissionClientAction::Select(TOOL_PERMISSION_ALLOW_OPTION_ID),
        ClientDecision::AllowAlways => PermissionClientAction::Select(TOOL_PERMISSION_ALLOW_ALWAYS_OPTION_ID),
        ClientDecision::Deny => PermissionClientAction::Select(TOOL_PERMISSION_DENY_OPTION_ID),
        ClientDecision::DenyAlways => PermissionClientAction::Select(TOOL_PERMISSION_DENY_ALWAYS_OPTION_ID),
        ClientDecision::Cancel => PermissionClientAction::Cancel,
        ClientDecision::Unknown => PermissionClientAction::Select("unsupported-option"),
        ClientDecision::RequestFailure => PermissionClientAction::Fail,
    }
}

fn test_prompter() -> DefaultPermissionPrompter<AcpToolRegistry> {
    DefaultPermissionPrompter::new(AcpToolRegistry::new(Path::new("/tmp"), true, true, Vec::new()))
}

struct RepeatedPermissionScenario<'a> {
    session_id: &'a acp::SessionId,
    call: &'a acp::ToolCall,
    args: &'a Value,
    second_args: Option<&'a Value>,
    force_prompt: bool,
}

async fn repeated_permission_results(
    prompter: &DefaultPermissionPrompter<AcpToolRegistry>,
    client: &ConnectionHandle,
    scenario: RepeatedPermissionScenario<'_>,
) -> Vec<Result<Option<ToolExecutionReport>, SdkError>> {
    let mut results = Vec::with_capacity(2);
    for index in 0..2 {
        let args = if index == 1 {
            scenario.second_args.unwrap_or(scenario.args)
        } else {
            scenario.args
        };
        let result = if scenario.force_prompt {
            prompter
                .request_tool_permission_forced(
                    client,
                    scenario.session_id,
                    scenario.call,
                    SupportedTool::ReadFile,
                    args,
                )
                .await
        } else {
            prompter
                .request_tool_permission(client, scenario.session_id, scenario.call, SupportedTool::ReadFile, args)
                .await
        };
        results.push(result);
    }
    results
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
                    ClientDecision::AllowAlways => RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new(TOOL_PERMISSION_ALLOW_ALWAYS_OPTION_ID),
                    )),
                    ClientDecision::Deny => RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new(TOOL_PERMISSION_DENY_OPTION_ID),
                    )),
                    ClientDecision::DenyAlways => RequestPermissionResponse::new(RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new(TOOL_PERMISSION_DENY_ALWAYS_OPTION_ID),
                    )),
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
            let results = repeated_permission_results(
                &prompter,
                &handle,
                RepeatedPermissionScenario {
                    session_id: &session_id,
                    call: &call,
                    args: &args,
                    second_args: second_args.as_ref(),
                    force_prompt,
                },
            )
            .await;
            drop(result_tx.send(results));
            Ok(())
        });

    let client = Client
        .builder()
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                let _previous_count = client_request_count.fetch_add(1, Ordering::Relaxed);
                assert_eq!(request.options.len(), 4);
                match permission_client_action(decision) {
                    PermissionClientAction::Select(option_id) => responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
                    )),
                    PermissionClientAction::Cancel => {
                        responder.respond(RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled))
                    }
                    PermissionClientAction::Fail => {
                        responder.respond_with_internal_error("simulated permission request failure")
                    }
                }
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
    let (results, request_count) = run_repeated_permission_flow(ClientDecision::AllowAlways, false, false, None).await;

    assert!(results.iter().all(Option::is_none));
    assert_eq!(request_count, 1);
}

#[tokio::test]
async fn reject_always_is_reused_for_the_session_tool() {
    let (results, request_count) = run_repeated_permission_flow(ClientDecision::DenyAlways, false, false, None).await;

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
    let (results, request_count) = run_repeated_permission_flow(ClientDecision::AllowAlways, false, true, None).await;

    assert!(results.iter().all(Option::is_none));
    assert_eq!(request_count, 2);
}

#[tokio::test]
async fn allow_always_does_not_cover_a_different_action_for_the_same_tool() {
    let (results, request_count) =
        run_repeated_permission_flow(ClientDecision::AllowAlways, false, false, Some(json!({ "path": "secrets.env" })))
            .await;

    assert!(results.iter().all(Option::is_none));
    assert_eq!(request_count, 2);
}

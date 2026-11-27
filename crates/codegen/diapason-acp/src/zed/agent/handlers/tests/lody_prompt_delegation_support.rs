//! Provider and update helpers shared by the Lody prompt-delegation tests.
//!
//! Every helper here installs or drives the mocked model's provider, or reads
//! the ACP updates that provider's call produces. The delegation cases and the
//! permission-hook rewrite cases share them, so both describe one wire
//! contract instead of keeping near-copies of it.

use super::*;
use std::time::Duration;

/// Arguments the mocked model emits for its single `agent` tool call.
const AGENT_TOOL_ARGUMENTS: &str = r#"{"action":"spawn_subprocess","agent_type":"background-demo"}"#;
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const POLL_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(25);
pub(super) const FAIL_CLOSED_MESSAGE: &str = "Subagent controller is not active in this session";

/// Installs the mocked agent-tool provider with the fixture's default call.
pub(super) fn install_agent_tool_provider()
-> (Arc<AtomicUsize>, Arc<Mutex<Vec<LLMRequest>>>, PromptProviderFactoryGuard) {
    install_agent_tool_provider_with_arguments(AGENT_TOOL_ARGUMENTS)
}

/// Installs the mocked agent-tool provider with caller-supplied arguments.
///
/// A test that needs the model to emit a specific target uses this instead of
/// the fixture default; the returned guards replace the fixture's provider for
/// the duration of the test.
pub(super) fn install_agent_tool_provider_with_arguments(
    tool_arguments: &str,
) -> (Arc<AtomicUsize>, Arc<Mutex<Vec<LLMRequest>>>, PromptProviderFactoryGuard) {
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let tool_arguments = tool_arguments.to_string();
    let guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(StreamToolThenAnswerProvider {
                emit_usage: false,
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_name: "agent",
                tool_arguments: tool_arguments.clone(),
                additional_tool_calls: Vec::new(),
            })
        }),
    );
    (calls, requests, guard)
}

/// Awaits the terminal ACP update for the mocked model's tool call.
pub(super) async fn terminal_tool_update(
    updates: &mut mpsc::UnboundedReceiver<acp::SessionNotification>,
) -> agent_client_protocol::schema::v1::ToolCallUpdate {
    terminal_update_in(&tool_notifications_through_terminal(updates).await)
}

/// Collects every notification up to and including the mocked model's
/// terminal ACP update.
///
/// Ordering is part of the contract a client observes: a call must be
/// announced before it settles. Retaining the notifications lets a test assert
/// that order instead of seeing only the final update.
pub(super) async fn tool_notifications_through_terminal(
    updates: &mut mpsc::UnboundedReceiver<acp::SessionNotification>,
) -> Vec<acp::SessionNotification> {
    tokio::time::timeout(POLL_TIMEOUT, async {
        let mut received = Vec::new();
        loop {
            let notification = updates.recv().await.expect("ACP session notification stream");
            let terminal = matches!(
                &notification.update,
                acp::SessionUpdate::ToolCallUpdate(update)
                    if update.tool_call_id.to_string() == "call-tool"
                        && matches!(
                            update.fields.status,
                            Some(acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed)
                        )
            );
            received.push(notification);
            if terminal {
                break received;
            }
        }
    })
    .await
    .expect("the prompt-driven tool call must report a terminal ACP update")
}

/// Extracts the terminal update from a collected notification history.
pub(super) fn terminal_update_in(
    notifications: &[acp::SessionNotification],
) -> agent_client_protocol::schema::v1::ToolCallUpdate {
    notifications
        .iter()
        .rev()
        .find_map(|notification| match &notification.update {
            acp::SessionUpdate::ToolCallUpdate(update)
                if update.tool_call_id.to_string() == "call-tool"
                    && matches!(
                        update.fields.status,
                        Some(acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed)
                    ) =>
            {
                Some(update.clone())
            }
            _ => None,
        })
        .expect("the prompt-driven tool call must report a terminal ACP update")
}

/// Asserts the client saw the call announced before it settled.
///
/// A rejected call is never `InProgress`, so the only well-formed sequence is
/// the initial `ToolCall` followed by the terminal `ToolCallUpdate`; a client
/// that receives the terminal update alone is being told about a call id it
/// never saw start.
pub(super) fn assert_call_announced_before_terminal_update(notifications: &[acp::SessionNotification], call_id: &str) {
    let announced = notifications
        .iter()
        .position(|notification| {
            matches!(
                &notification.update,
                acp::SessionUpdate::ToolCall(call) if call.tool_call_id.to_string() == call_id
            )
        })
        .unwrap_or_else(|| panic!("the call {call_id} must be announced with an initial ToolCall"));
    let settled = notifications
        .iter()
        .rposition(|notification| {
            matches!(
                &notification.update,
                acp::SessionUpdate::ToolCallUpdate(update)
                    if update.tool_call_id.to_string() == call_id
                        && matches!(
                            update.fields.status,
                            Some(acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed)
                        )
            )
        })
        .expect("the call must report a terminal update");
    assert!(
        announced < settled,
        "the initial ToolCall for {call_id} must precede its terminal update: \
         announced at {announced}, settled at {settled}"
    );
}

pub(super) fn tool_update_text(update: &agent_client_protocol::schema::v1::ToolCallUpdate) -> String {
    update
        .fields
        .content
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|content| match content {
            acp::ToolCallContent::Content(content) => match &content.content {
                acp::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Asserts the mocked tool call ran, and whether the model saw the tool offered.
///
/// The `agent` tool is advertised from the session's own subagent controller,
/// so a fail-closed session must not offer it even though the mocked model
/// emits the call regardless. Pass `advertised: false` for those cases: they
/// still require the call to reach the dispatcher and settle there, which is
/// the guarantee the fail-closed path exists to keep.
pub(super) fn assert_tool_reached_the_model(
    provider_calls: &AtomicUsize,
    provider_requests: &Mutex<Vec<LLMRequest>>,
    advertised: bool,
) {
    assert_eq!(provider_calls.load(Ordering::SeqCst), 2, "the tool result must return to the provider");
    let requests = provider_requests.lock().expect("read provider requests");
    let offered = requests[0]
        .tools
        .as_deref()
        .is_some_and(|definitions| definitions.iter().any(|tool| tool.function_name() == "agent"));
    assert_eq!(
        offered,
        advertised,
        "a session {} advertise the agent tool",
        if advertised {
            "with a subagent controller must"
        } else {
            "without a subagent controller must not"
        }
    );
    assert!(
        requests[1].messages.iter().any(|message| message.is_tool_response()),
        "the second provider request must contain the decoded ACP tool result"
    );
}

pub(super) fn assert_tool_response_contains(provider_requests: &Mutex<Vec<LLMRequest>>, expected: &str) {
    let requests = provider_requests.lock().expect("read provider requests");
    let response = requests[1]
        .messages
        .iter()
        .find(|message| message.is_tool_response())
        .map(|message| message.content.as_text())
        .expect("agent tool response");
    assert!(response.contains(expected), "the model must receive the tool result: {response}");
}

pub(super) fn delegation_prompt(session_id: acp::SessionId) -> PromptRequest {
    PromptRequest::new(
        session_id,
        vec![acp::ContentBlock::Text(acp::TextContent::new(
            "Delegate a background task",
        ))],
    )
}

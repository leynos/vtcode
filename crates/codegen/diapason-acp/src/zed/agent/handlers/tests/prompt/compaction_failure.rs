//! Covers the terminal activity a failed or cancelled ACP compaction publishes.
//!
//! The helpers here drive one compaction attempt over a real duplex ACP
//! transport; the submodules group the assertions by the boundary they
//! observe: the terminal activity and its failure category, the send boundary
//! that publishes an update, and the metrics that record an attempt.
use super::compaction::{CaptureWriter, capture_subscriber};
use super::*;
use crate::zed::agent::compaction::CompactionActivitySource;
use agent_client_protocol::schema::v1::InitializeRequest;

/// Provider stall the duration assertions are bounded by, in milliseconds.
const PROVIDER_DELAY_MS: u64 = 20;

/// Assert the started/terminal activity pair a failed compaction publishes.
fn assert_failed_terminal_activity(updates: &[serde_json::Value], expected_category: &str) {
    assert_eq!(updates.len(), 2, "a failed compaction must publish exactly one started and one terminal activity");
    let started = &updates[0];
    let finished = &updates[1];
    assert_eq!(started["sessionUpdate"], "tool_call");
    assert_eq!(started["status"], "in_progress");
    assert_eq!(started["_meta"]["lody"]["activity"]["kind"], "context_compaction");
    assert_eq!(finished["sessionUpdate"], "tool_call_update");
    assert_eq!(finished["status"], "failed");
    assert_eq!(
        started["toolCallId"], finished["toolCallId"],
        "the failed terminal update must keep the started tool call ID"
    );
    assert_eq!(finished["_meta"]["lody"]["activity"]["kind"], "context_compaction");
    assert_eq!(finished["_meta"]["lody"]["activity"]["compacted"], false);
    assert!(
        finished["_meta"]["lody"]["activity"].get("usedTokensAfter").is_none(),
        "a failed compaction must not report a post-compaction token count"
    );
    assert_eq!(
        finished["_meta"]["lody"]["activity"]["failureReason"], expected_category,
        "the terminal update must carry the closed-set failure category"
    );
}

/// Run one compaction attempt against a real duplex ACP transport.
///
/// The transport is the only way to observe the activity notifications a
/// client receives, so this drives the same `maybe_compact_session` entry
/// point the prompt handler uses and returns every published activity update
/// alongside the failure that ended the attempt.
async fn run_failing_compaction_over_duplex(
    agent: &Arc<ZedAgent>,
    session: &SessionHandle,
    provider: Arc<dyn LLMProvider>,
    started: Arc<tokio::sync::Notify>,
    cancel_after_start: bool,
) -> (Vec<serde_json::Value>, anyhow::Error) {
    let (agent_channel, client_channel) = Channel::duplex();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_notifications = Arc::clone(&captured);
    let agent_attached = Arc::new(tokio::sync::Notify::new());
    let client_initialized = Arc::new(tokio::sync::Notify::new());
    let agent_attached_signal = Arc::clone(&agent_attached);
    let client_initialized_signal = Arc::clone(&client_initialized);

    let agent_connection = install_handlers(
        Agent.builder().name("diapason-compaction-failure-test"),
        Arc::clone(agent),
    )
    .connect_with(agent_channel, {
        let agent = Arc::clone(agent);
        async move |cx: ConnectionTo<Client>| {
            agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
            agent_attached_signal.notify_one();
            std::future::pending::<agent_client_protocol::Result<()>>().await
        }
    });
    let agent_task = tokio::spawn(agent_connection);

    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                captured_notifications
                    .lock()
                    .expect("capture compaction notifications")
                    .push(notification);
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            client_initialized_signal.notify_one();
            std::future::pending::<agent_client_protocol::Result<()>>().await
        });
    let client_task = tokio::spawn(client_connection);

    tokio::time::timeout(Duration::from_secs(5), async {
        agent_attached.notified().await;
        client_initialized.notified().await;
    })
    .await
    .expect("the ACP test transport should attach the client and finish initialisation");

    let runtime = agent.provider_runtime.for_provider("wire-test");
    let task = {
        let agent = Arc::clone(agent);
        let session = session.clone();
        tokio::spawn(async move {
            agent
                .maybe_compact_session(CompactionRequest {
                    session: &session,
                    provider: provider.as_ref(),
                    runtime: &runtime,
                    model: "wire-model",
                    tools: None,
                })
                .await
        })
    };

    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("provider compaction request should start");
    if cancel_after_start {
        session.cancellation.cancel();
    }
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("compaction should finish")
        .expect("compaction task should not panic")
        .expect_err("the fixture provider must fail compaction");

    let updates = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let updates = captured
                .lock()
                .expect("capture compaction notifications")
                .iter()
                .filter_map(|notification| serde_json::to_value(&notification.update).ok())
                .filter(|update| update["_meta"]["lody"]["activity"].is_object())
                .collect::<Vec<_>>();
            if updates.len() >= 2 {
                break updates;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a failed compaction must publish a terminal activity update");

    agent_task.abort();
    client_task.abort();
    drop(agent_task.await);
    drop(client_task.await);

    (updates, error)
}

#[path = "compaction_failure/failure_updates.rs"]
mod failure_updates;

#[path = "compaction_failure/metrics.rs"]
mod metrics;

#[path = "compaction_failure/publication.rs"]
mod publication;

//! Projects automatic context-compaction activity into standard ACP tool-call
//! updates carrying `_meta.lody.activity`, and advertises the matching
//! capability so clients can render compaction progress.

use crate::acp;
use serde::Serialize;
use serde_json::{Map, Value};
use std::time::{Duration, Instant};

use super::lody::lody_capabilities_mut;

const LODY_EXTENSION_VERSION: u8 = 1;
const COMPACTION_TITLE: &str = "Compact conversation context";

#[derive(Debug)]
pub(super) struct CompactionActivity {
    tool_call_id: String,
    used_tokens_before: usize,
    started_at: Instant,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LodyActivityMeta<'a> {
    version: u8,
    kind: &'static str,
    automatic: bool,
    used_tokens_before: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    used_tokens_after: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compacted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_reason: Option<&'a str>,
}

impl CompactionActivity {
    /// Start a compaction activity from a caller-supplied identity and clock.
    ///
    /// The identifier and start instant are inputs rather than ambient
    /// dependencies, so the projection stays pure and an edge adapter owns the
    /// random and monotonic-clock choices. Tests can then supply fixed values
    /// and assert exact durations.
    pub(super) fn begin(
        session_id: &acp::SessionId,
        used_tokens_before: usize,
        activity_id: uuid::Uuid,
        started_at: Instant,
    ) -> Self {
        Self {
            tool_call_id: format!("activity:compaction:{}:{activity_id}", session_id.0),
            used_tokens_before,
            started_at,
        }
    }

    /// Report how long this activity has been running at `now`.
    pub(super) fn elapsed_millis(&self, now: Instant) -> u64 {
        duration_millis(now.saturating_duration_since(self.started_at))
    }

    /// Build the in-progress `tool_call` update that announces this attempt.
    pub(super) fn started_update(&self) -> anyhow::Result<acp::SessionUpdate> {
        let meta = activity_meta(LodyActivityMeta {
            version: LODY_EXTENSION_VERSION,
            kind: "context_compaction",
            automatic: true,
            used_tokens_before: self.used_tokens_before,
            used_tokens_after: None,
            compacted: None,
            duration_ms: None,
            failure_reason: None,
        })?;
        Ok(acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(self.tool_call_id.clone(), COMPACTION_TITLE)
                .kind(acp::ToolKind::Think)
                .status(acp::ToolCallStatus::InProgress)
                .meta(meta),
        ))
    }

    /// Build the terminal `tool_call_update` for this attempt at `now`.
    ///
    /// A failure reason makes the update `failed`; otherwise it reports the
    /// outcome as completed, including an attempt that changed nothing.
    pub(super) fn finished_update(
        &self,
        now: Instant,
        compacted: bool,
        used_tokens_after: Option<usize>,
        failure_reason: Option<&str>,
    ) -> anyhow::Result<acp::SessionUpdate> {
        let status = if failure_reason.is_some() {
            acp::ToolCallStatus::Failed
        } else {
            acp::ToolCallStatus::Completed
        };
        let meta = activity_meta(LodyActivityMeta {
            version: LODY_EXTENSION_VERSION,
            kind: "context_compaction",
            automatic: true,
            used_tokens_before: self.used_tokens_before,
            used_tokens_after,
            compacted: Some(compacted),
            duration_ms: Some(duration_millis(now.saturating_duration_since(self.started_at))),
            failure_reason,
        })?;
        let fields = acp::ToolCallUpdateFields::new()
            .title(COMPACTION_TITLE)
            .kind(acp::ToolKind::Think)
            .status(status);
        Ok(acp::SessionUpdate::ToolCallUpdate(
            acp::ToolCallUpdate::new(self.tool_call_id.clone(), fields).meta(meta),
        ))
    }
}

/// Advertise the `_meta.lody.compaction` extension during negotiation.
pub(super) fn add_lody_compaction_capability(capabilities: &mut acp::AgentCapabilities) {
    if let Some(lody) = lody_capabilities_mut(capabilities) {
        let _ = lody.insert("compaction".to_string(), serde_json::json!({ "version": LODY_EXTENSION_VERSION }));
    }
}

/// Wrap an activity snapshot in the `_meta.lody.activity` envelope.
fn activity_meta(activity: LodyActivityMeta<'_>) -> anyhow::Result<Map<String, Value>> {
    let mut lody = Map::new();
    let _ = lody.insert("activity".to_string(), serde_json::to_value(activity)?);
    let mut meta = Map::new();
    let _ = meta.insert("lody".to_string(), Value::Object(lody));
    Ok(meta)
}

/// Convert a duration to whole milliseconds, saturating on overflow.
fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zed::connection::ConnectionHandle;
    use agent_client_protocol::{Agent, Builder, Client, ConnectionTo, RunWithConnectionTo, on_receive_notification};
    use std::sync::Arc;
    use tokio::sync::{Notify, mpsc};

    /// Assert compaction uses standard tool calls carrying Lody activity metadata.
    #[test]
    fn compaction_updates_use_standard_tool_lifecycle_with_lody_activity_meta() {
        let activity = CompactionActivity {
            tool_call_id: "activity:compaction:test".to_string(),
            used_tokens_before: 120_000,
            started_at: Instant::now(),
        };

        let started = serde_json::to_value(activity.started_update().expect("started update")).expect("started JSON");
        assert_eq!(started["sessionUpdate"], "tool_call");
        assert_eq!(started["status"], "in_progress");
        assert_eq!(started["_meta"]["lody"]["activity"]["kind"], "context_compaction");
        assert_eq!(started["_meta"]["lody"]["activity"]["usedTokensBefore"], 120_000);

        let finished_at = activity.started_at + Duration::from_millis(250);
        let finished = serde_json::to_value(
            activity
                .finished_update(finished_at, true, Some(42_000), None)
                .expect("finished update"),
        )
        .expect("finished JSON");
        assert_eq!(finished["sessionUpdate"], "tool_call_update");
        assert_eq!(finished["status"], "completed");
        assert_eq!(finished["_meta"]["lody"]["activity"]["compacted"], true);
        assert_eq!(finished["_meta"]["lody"]["activity"]["usedTokensAfter"], 42_000);
        assert_eq!(finished["_meta"]["lody"]["activity"]["durationMs"], 250);
    }

    /// Assert a no-op compaction reports not-compacted without a post token count.
    #[test]
    fn no_op_compaction_is_reported_as_not_compacted_without_token_after() {
        let activity = CompactionActivity {
            tool_call_id: "activity:compaction:noop".to_string(),
            used_tokens_before: 12_000,
            started_at: Instant::now(),
        };

        let finished_at = activity.started_at + Duration::from_millis(10);
        let finished = serde_json::to_value(
            activity
                .finished_update(finished_at, false, None, None)
                .expect("no-op finished update"),
        )
        .expect("no-op finished JSON");

        assert_eq!(finished["status"], "completed");
        assert_eq!(finished["_meta"]["lody"]["activity"]["compacted"], false);
        assert!(finished["_meta"]["lody"]["activity"].get("usedTokensAfter").is_none());
    }

    /// Assert a failed compaction update carries its failure reason.
    #[test]
    fn failed_compaction_update_carries_the_failure_reason() {
        let activity = CompactionActivity::begin(
            &acp::SessionId::new(Arc::from("session-a")),
            32_000,
            uuid::Uuid::nil(),
            Instant::now(),
        );
        let update = serde_json::to_value(
            activity
                .finished_update(Instant::now(), false, None, Some("provider timed out"))
                .expect("failed update"),
        )
        .expect("failed JSON");

        assert_eq!(update["status"], "failed");
        assert_eq!(update["_meta"]["lody"]["activity"]["compacted"], false);
        assert_eq!(update["_meta"]["lody"]["activity"]["failureReason"], "provider timed out");
    }

    /// Assert the compaction activity pair round-trips over the ACP duplex.
    #[tokio::test]
    async fn compaction_lifecycle_round_trips_over_the_official_acp_duplex() {
        let (agent_channel, client_channel) = agent_client_protocol::Channel::duplex();
        let (received_tx, mut received_rx) = mpsc::unbounded_channel();
        let session_id = acp::SessionId::new(Arc::from("session-compaction"));
        let activity = CompactionActivity {
            tool_call_id: "activity:compaction:wire".to_string(),
            used_tokens_before: 90_000,
            started_at: Instant::now(),
        };
        let client_ready = Arc::new(Notify::new());
        let agent_ready = Arc::clone(&client_ready);
        let notifications_received = Arc::new(Notify::new());
        let agent_ack = Arc::clone(&notifications_received);

        let agent_connection = Agent.builder().name("vtcode-compaction-test").connect_with(
            agent_channel,
            move |cx: ConnectionTo<Client>| async move {
                agent_ready.notified().await;
                let client = ConnectionHandle::new(cx);
                for update in [
                    activity.started_update().expect("start update"),
                    activity
                        .finished_update(activity.started_at, true, Some(30_000), None)
                        .expect("terminal update"),
                ] {
                    client
                        .send_session_notification(acp::SessionNotification::new(session_id.clone(), update))
                        .expect("send compaction update");
                }
                agent_ack.notified().await;
                Ok(())
            },
        );
        let agent_task = tokio::spawn(agent_connection);

        let client_connection = Client
            .builder()
            .on_receive_notification(
                async move |notification: acp::AgentNotification, _cx| {
                    drop(received_tx.send(notification));
                    Ok(())
                },
                on_receive_notification!(),
            )
            .connect_with(client_channel, async move |_cx: ConnectionTo<Agent>| {
                client_ready.notify_one();
                let first = received_rx.recv().await.expect("start notification");
                let second = received_rx.recv().await.expect("terminal notification");
                let acp::AgentNotification::SessionNotification(first) = first else {
                    panic!("compaction start must use a standard session notification");
                };
                let acp::AgentNotification::SessionNotification(second) = second else {
                    panic!("compaction terminal state must use a standard session notification");
                };
                assert!(matches!(first.update, acp::SessionUpdate::ToolCall(_)));
                assert!(matches!(second.update, acp::SessionUpdate::ToolCallUpdate(_)));
                notifications_received.notify_one();
                Ok(())
            });

        tokio::time::timeout(Duration::from_secs(3), client_connection)
            .await
            .expect("client connection deadline")
            .expect("client connection");
        agent_task.await.expect("agent task").expect("agent connection");
    }
}

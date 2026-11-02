//! Publishes usage deltas and advertises the Lody usage capability, including
//! the `_lody/session/usage_update` extension notification.

use crate::acp;
#[cfg(test)]
use crate::zed::connection::ConnectionHandle;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;
use tracing::{field, info_span, warn};
use vtcode_core::llm::Usage;

use super::task_lifecycle::bounded_session_correlation;

use super::{ZedAgent, lody::lody_capabilities_mut};

const LODY_EXTENSION_VERSION: u8 = 1;
pub(super) const LODY_SESSION_USAGE_UPDATE_METHOD: &str = "_lody/session/usage_update";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct LodyUsage {
    input_tokens: u32,
    output_tokens: u32,
    cache_read_input_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_creation_input_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LodySessionUsageUpdate<'a> {
    session_id: &'a str,
    usage: LodyUsage,
    model_usage: BTreeMap<&'a str, LodyUsage>,
}

pub(super) fn add_lody_usage_capability(capabilities: &mut acp::AgentCapabilities) {
    let usage = serde_json::json!({ "version": LODY_EXTENSION_VERSION });
    if let Some(lody) = lody_capabilities_mut(capabilities) {
        let _ = lody.insert("usage".to_string(), usage);
    }
}

fn usage_notification(session_id: &acp::SessionId, model: &str, usage: &Usage) -> anyhow::Result<acp::ExtNotification> {
    let delta = LodyUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        cache_read_input_tokens: usage.cache_read_tokens_or_fallback(),
        cache_creation_input_tokens: usage.cache_creation_tokens,
    };
    let payload = LodySessionUsageUpdate {
        session_id: session_id.0.as_ref(),
        usage: delta.clone(),
        model_usage: BTreeMap::from([(model, delta)]),
    };
    let params = serde_json::value::to_raw_value(&payload)?;
    Ok(acp::ExtNotification::new(LODY_SESSION_USAGE_UPDATE_METHOD, Arc::from(params)))
}

fn response_usage_notification(
    session_id: &acp::SessionId,
    fallback_model: &str,
    response: &vtcode_core::llm::provider::LLMResponse,
) -> anyhow::Result<Option<acp::ExtNotification>> {
    let Some(usage) = response.usage.as_ref() else {
        return Ok(None);
    };
    let model = if response.model.is_empty() {
        fallback_model
    } else {
        response.model.as_str()
    };
    usage_notification(session_id, model, usage).map(Some)
}

#[cfg(test)]
fn send_usage_update(
    client: &ConnectionHandle,
    session_id: &acp::SessionId,
    model: &str,
    usage: &Usage,
) -> anyhow::Result<()> {
    client
        .send_ext_notification(usage_notification(session_id, model, usage)?)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn usage_elapsed_ms(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Records a failed usage publication on its span and in the bounded metrics.
///
/// The session is identified only by its bounded correlation hash, and the
/// error text carries no usage payload, so the log stays safe to retain.
fn record_usage_failure(
    span: &tracing::Span,
    started_at: Instant,
    metrics: &vtcode_core::metrics::MetricsCollector,
    error_category: &'static str,
    session_correlation: &str,
    error: &dyn std::fmt::Display,
) {
    let elapsed_ms = usage_elapsed_ms(started_at);
    let _outcome_recorded = span.record("outcome", "failed");
    let _elapsed_ms_recorded = span.record("elapsed_ms", elapsed_ms);
    let _error_category_recorded = span.record("error_category", error_category);
    metrics.record_acp_usage_notification(false);
    warn!(session_correlation, error_category, elapsed_ms, %error, "Failed to publish Lody ACP usage update");
}

impl ZedAgent {
    pub(super) fn publish_lody_usage(
        &self,
        session_id: &acp::SessionId,
        fallback_model: &str,
        response: &vtcode_core::llm::provider::LLMResponse,
    ) {
        let Some(client) = self.client() else {
            return;
        };
        let metrics = self.local_tool_registry.metrics_collector();
        let session_correlation = bounded_session_correlation(session_id);
        let span = info_span!(
            "acp_lody_usage_notification",
            session_correlation = %session_correlation,
            outcome = field::Empty,
            elapsed_ms = field::Empty,
            error_category = field::Empty
        );
        let _span_entered = span.enter();
        let started_at = Instant::now();
        let notification = match response_usage_notification(session_id, fallback_model, response) {
            Ok(Some(notification)) => notification,
            Ok(None) => {
                let _outcome_recorded = span.record("outcome", "no_usage");
                let _elapsed_ms_recorded = span.record("elapsed_ms", usage_elapsed_ms(started_at));
                return;
            }
            Err(error) => {
                record_usage_failure(&span, started_at, &metrics, "serialization_failed", &session_correlation, &error);
                return;
            }
        };
        if let Err(error) = client.send_ext_notification(notification) {
            record_usage_failure(&span, started_at, &metrics, "notification_send_failed", &session_correlation, &error);
            return;
        }
        let _outcome_recorded = span.record("outcome", "sent");
        let _elapsed_ms_recorded = span.record("elapsed_ms", usage_elapsed_ms(started_at));
        metrics.record_acp_usage_notification(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::{Agent, Builder, Client, ConnectionTo, RunWithConnectionTo, on_receive_notification};
    use proptest::prelude::*;
    use tokio::sync::{Notify, mpsc};

    #[test]
    fn maps_normalized_usage_to_the_lody_delta_contract() {
        let usage = Usage {
            prompt_tokens: 21,
            completion_tokens: 8,
            cached_prompt_tokens: Some(5),
            cache_creation_tokens: Some(3),
            ..Usage::default()
        };
        let notification = usage_notification(&acp::SessionId::new(Arc::from("session-1")), "model-a", &usage)
            .expect("usage notification");
        let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");

        assert_eq!(notification.method.as_ref(), LODY_SESSION_USAGE_UPDATE_METHOD);
        assert_eq!(value["sessionId"], "session-1");
        assert_eq!(value["usage"]["inputTokens"], 21);
        assert_eq!(value["usage"]["outputTokens"], 8);
        assert_eq!(value["usage"]["cacheReadInputTokens"], 5);
        assert_eq!(value["usage"]["cacheCreationInputTokens"], 3);
        assert_eq!(value["modelUsage"]["model-a"], value["usage"]);
    }

    #[test]
    fn response_without_usage_produces_no_notification() {
        let response = vtcode_core::llm::provider::LLMResponse::new("model-a", "answer");

        let notification =
            response_usage_notification(&acp::SessionId::new(Arc::from("session-1")), "fallback-model", &response)
                .expect("optional usage notification");

        assert!(notification.is_none());
    }

    proptest! {
        #[test]
        fn preserves_all_normalized_u32_usage_counts(
            input in any::<u32>(),
            output in any::<u32>(),
            cache_read in any::<u32>(),
            cache_creation in proptest::option::of(any::<u32>()),
            model in "[^\\p{C}]{1,48}",
        ) {
            let usage = Usage {
                prompt_tokens: input,
                completion_tokens: output,
                cache_read_tokens: Some(cache_read),
                cache_creation_tokens: cache_creation,
                ..Usage::default()
            };
            let notification = usage_notification(
                &acp::SessionId::new(Arc::from("property-session")),
                &model,
                &usage,
            ).expect("usage notification");
            let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");

            prop_assert_eq!(value["usage"]["inputTokens"].as_u64(), Some(u64::from(input)));
            prop_assert_eq!(value["usage"]["outputTokens"].as_u64(), Some(u64::from(output)));
            prop_assert_eq!(value["usage"]["cacheReadInputTokens"].as_u64(), Some(u64::from(cache_read)));
            match cache_creation {
                Some(count) => prop_assert_eq!(
                    value["usage"]["cacheCreationInputTokens"].as_u64(),
                    Some(u64::from(count)),
                ),
                None => prop_assert!(value["usage"].get("cacheCreationInputTokens").is_none()),
            }
            prop_assert_eq!(&value["modelUsage"][model.as_str()], &value["usage"]);
        }
    }

    #[tokio::test]
    async fn sends_usage_through_the_official_acp_extension_channel() {
        let (agent_channel, client_channel) = agent_client_protocol::Channel::duplex();
        let (received_tx, mut received_rx) = mpsc::unbounded_channel();
        let client_ready = Arc::new(Notify::new());
        let agent_ready = Arc::clone(&client_ready);
        let notification_received = Arc::new(Notify::new());
        let agent_ack = Arc::clone(&notification_received);

        let agent_connection = Agent.builder().name("vtcode-usage-test").connect_with(
            agent_channel,
            move |cx: ConnectionTo<Client>| async move {
                agent_ready.notified().await;
                let client = ConnectionHandle::new(cx);
                send_usage_update(
                    &client,
                    &acp::SessionId::new(Arc::from("session-wire")),
                    "wire-model",
                    &Usage {
                        prompt_tokens: 13,
                        completion_tokens: 5,
                        ..Usage::default()
                    },
                )
                .expect("send usage update");
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
                let notification = tokio::time::timeout(std::time::Duration::from_secs(2), received_rx.recv())
                    .await
                    .expect("usage notification deadline")
                    .expect("usage notification");
                let acp::AgentNotification::ExtNotification(notification) = notification else {
                    panic!("usage must use an ACP extension notification");
                };
                assert_eq!(
                    notification.method.as_ref().trim_start_matches('_'),
                    LODY_SESSION_USAGE_UPDATE_METHOD.trim_start_matches('_')
                );
                let value: serde_json::Value =
                    serde_json::from_str(notification.params.get()).expect("usage notification JSON");
                assert_eq!(value["sessionId"], "session-wire");
                assert_eq!(value["usage"]["inputTokens"], 13);
                assert_eq!(value["usage"]["outputTokens"], 5);
                notification_received.notify_one();
                Ok(())
            });

        tokio::time::timeout(std::time::Duration::from_secs(3), client_connection)
            .await
            .expect("client connection deadline")
            .expect("client connection");
        agent_task.await.expect("agent task").expect("agent connection");
    }
}

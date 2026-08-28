use crate::acp;
#[cfg(test)]
use crate::zed::connection::ConnectionHandle;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use vtcode_config::models::ModelPricing;
use vtcode_core::llm::Usage;
use vtcode_core::llm::model_resolver::ModelResolver;
use vtcode_core::llm::usage_cost::{estimate_session_costs_with_pricing, normalized_turn_usage};

use super::{ZedAgent, lody::lody_capabilities_mut};

const LODY_EXTENSION_VERSION: u8 = 1;
pub(super) const LODY_SESSION_USAGE_UPDATE_METHOD: &str = "_lody/session/usage_update";

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct LodyUsage {
    input_tokens: u32,
    output_tokens: u32,
    cache_read_input_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_creation_input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_output_tokens: Option<u32>,
    #[serde(rename = "costUSD", skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_window: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct UsageProfile {
    context_window: Option<usize>,
    pricing: Option<ModelPricing>,
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

fn usage_notification(
    session_id: &acp::SessionId,
    provider: &str,
    model: &str,
    usage: &Usage,
    profile: UsageProfile,
) -> anyhow::Result<acp::ExtNotification> {
    let reasoning_output_tokens = usage
        .reasoning_output_tokens
        .map(|reasoning| reasoning.min(usage.completion_tokens));
    let output_tokens = usage
        .completion_tokens
        .saturating_sub(reasoning_output_tokens.unwrap_or_default());
    let cost_usd = profile.pricing.and_then(|pricing| {
        let normalized = normalized_turn_usage(provider, usage);
        estimate_session_costs_with_pricing(pricing, &normalized).map(|estimate| estimate.effective_usd)
    });
    let delta = LodyUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens,
        cache_read_input_tokens: usage.cache_read_tokens_or_fallback(),
        cache_creation_input_tokens: usage.cache_creation_tokens,
        reasoning_output_tokens,
        cost_usd,
        context_window: profile.context_window,
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
    provider: &str,
    fallback_model: &str,
    response: &vtcode_core::llm::provider::LLMResponse,
    profile: UsageProfile,
) -> anyhow::Result<Option<acp::ExtNotification>> {
    let Some(usage) = response.usage.as_ref() else {
        return Ok(None);
    };
    let model = if response.model.is_empty() {
        fallback_model
    } else {
        response.model.as_str()
    };
    usage_notification(session_id, provider, model, usage, profile).map(Some)
}

#[cfg(test)]
fn send_usage_update(
    client: &ConnectionHandle,
    session_id: &acp::SessionId,
    provider: &str,
    model: &str,
    usage: &Usage,
    profile: UsageProfile,
) -> anyhow::Result<()> {
    client
        .send_ext_notification(usage_notification(session_id, provider, model, usage, profile)?)
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

impl ZedAgent {
    fn lody_usage_profile(&self, provider: &str, model: &str) -> UsageProfile {
        if let Some(custom_provider) = self
            .workspace_runtime_config
            .custom_providers
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(provider))
        {
            let resolved = custom_provider.resolved_profile(model);
            return UsageProfile {
                context_window: resolved.context_window.filter(|window| *window > 0),
                pricing: resolved.pricing.model_pricing(),
            };
        }

        ModelResolver::resolve(Some(provider), model, &[], None).map_or_else(UsageProfile::default, |resolved| {
            UsageProfile {
                context_window: resolved.context_window(),
                pricing: resolved.pricing(),
            }
        })
    }

    pub(super) fn publish_lody_usage(
        &self,
        session_id: &acp::SessionId,
        provider: &str,
        fallback_model: &str,
        response: &vtcode_core::llm::provider::LLMResponse,
    ) {
        let Some(client) = self.client() else {
            return;
        };
        let profile = self.lody_usage_profile(provider, fallback_model);
        let notification = match response_usage_notification(session_id, provider, fallback_model, response, profile) {
            Ok(Some(notification)) => notification,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, %session_id, "Failed to serialize Lody ACP usage update");
                return;
            }
        };
        if let Err(error) = client.send_ext_notification(notification) {
            tracing::warn!(%error, %session_id, "Failed to publish Lody ACP usage update");
        }
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
            reasoning_output_tokens: Some(3),
            cached_prompt_tokens: Some(5),
            cache_creation_tokens: Some(3),
            ..Usage::default()
        };
        let notification = usage_notification(
            &acp::SessionId::new(Arc::from("session-1")),
            "baseten",
            "model-a",
            &usage,
            UsageProfile {
                context_window: Some(1_048_576),
                pricing: Some(ModelPricing {
                    input: Some(0.15 / 1_000_000.0),
                    output: Some(0.50 / 1_000_000.0),
                    cache_read: Some(0.03 / 1_000_000.0),
                    cache_write: None,
                }),
            },
        )
        .expect("usage notification");
        let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");

        assert_eq!(notification.method.as_ref(), LODY_SESSION_USAGE_UPDATE_METHOD);
        assert_eq!(value["sessionId"], "session-1");
        assert_eq!(value["usage"]["inputTokens"], 21);
        assert_eq!(value["usage"]["outputTokens"], 5);
        assert_eq!(value["usage"]["reasoningOutputTokens"], 3);
        assert_eq!(value["usage"]["cacheReadInputTokens"], 5);
        assert_eq!(value["usage"]["cacheCreationInputTokens"], 3);
        assert_eq!(value["usage"]["contextWindow"], 1_048_576);
        assert!(value["usage"]["costUSD"].as_f64().is_some_and(|cost| cost > 0.0));
        assert_eq!(value["modelUsage"]["model-a"], value["usage"]);
    }

    #[test]
    fn response_without_usage_produces_no_notification() {
        let response = vtcode_core::llm::provider::LLMResponse::new("model-a", "answer");

        let notification = response_usage_notification(
            &acp::SessionId::new(Arc::from("session-1")),
            "baseten",
            "fallback-model",
            &response,
            UsageProfile::default(),
        )
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
            raw_reasoning in any::<u32>(),
            model in "[^\\p{C}]{1,48}",
        ) {
            let usage = Usage {
                prompt_tokens: input,
                completion_tokens: output,
                reasoning_output_tokens: Some(raw_reasoning),
                cache_read_tokens: Some(cache_read),
                cache_creation_tokens: cache_creation,
                ..Usage::default()
            };
            let notification = usage_notification(
                &acp::SessionId::new(Arc::from("property-session")),
                "baseten",
                &model,
                &usage,
                UsageProfile::default(),
            ).expect("usage notification");
            let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");

            prop_assert_eq!(value["usage"]["inputTokens"].as_u64(), Some(u64::from(input)));
            prop_assert_eq!(
                value["usage"]["outputTokens"].as_u64(),
                Some(u64::from(output.saturating_sub(raw_reasoning.min(output)))),
            );
            prop_assert_eq!(
                value["usage"]["reasoningOutputTokens"].as_u64(),
                Some(u64::from(raw_reasoning.min(output))),
            );
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
                    "baseten",
                    "wire-model",
                    &Usage {
                        prompt_tokens: 13,
                        completion_tokens: 5,
                        ..Usage::default()
                    },
                    UsageProfile { context_window: Some(524_288), pricing: None },
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
                assert_eq!(value["usage"]["contextWindow"], 524_288);
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

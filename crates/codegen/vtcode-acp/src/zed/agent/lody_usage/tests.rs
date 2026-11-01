//! Tests for the Lody usage extension notification.

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
            exclusive_input: false,
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
fn backend_selects_input_semantics_over_the_provider_name() {
    let cases = [
        (Some(CustomProviderApiFormat::AnthropicMessages), "baseten", true),
        (Some(CustomProviderApiFormat::OpenAIChat), "anthropic", false),
        (Some(CustomProviderApiFormat::OpenAIResponses), "anthropic", false),
        (Some(CustomProviderApiFormat::Auto), "anthropic", true),
        (Some(CustomProviderApiFormat::Auto), "baseten", false),
        (None, "anthropic", true),
    ];

    for (api_format, provider, expected) in cases {
        assert_eq!(
            exclusive_input_semantics(api_format, provider),
            expected,
            "api_format {api_format:?} for provider {provider}"
        );
    }
}

#[test]
fn cost_normalization_follows_the_selected_backend_not_the_provider_name() {
    // `baseten` is a custom name either way; only the backend decides
    // whether `prompt_tokens` already includes the cached tokens.
    let usage = Usage {
        prompt_tokens: 100,
        completion_tokens: 0,
        cached_prompt_tokens: Some(50),
        ..Usage::default()
    };
    let pricing = Some(ModelPricing {
        input: Some(1.0 / 1_000_000.0),
        output: Some(0.0),
        cache_read: Some(0.0),
        cache_write: Some(0.0),
    });
    let cost = |exclusive_input| {
        let notification = usage_notification(
            &acp::SessionId::new(Arc::from("session-cost")),
            "model-a",
            &usage,
            UsageProfile {
                pricing,
                exclusive_input,
                ..UsageProfile::default()
            },
        )
        .expect("usage notification");
        let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");
        value["usage"]["costUSD"].as_f64().expect("costUSD")
    };

    assert!((cost(false) - 50.0 / 1_000_000.0).abs() < f64::EPSILON);
    assert!((cost(true) - 100.0 / 1_000_000.0).abs() < f64::EPSILON);
}

#[test]
fn response_without_usage_produces_no_notification() {
    let response = vtcode_core::llm::provider::LLMResponse::new("model-a", "answer");

    let notification = response_usage_notification(
        &acp::SessionId::new(Arc::from("session-1")),
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
                "wire-model",
                &Usage {
                    prompt_tokens: 13,
                    completion_tokens: 5,
                    ..Usage::default()
                },
                UsageProfile {
                    context_window: Some(524_288),
                    pricing: None,
                    exclusive_input: false,
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

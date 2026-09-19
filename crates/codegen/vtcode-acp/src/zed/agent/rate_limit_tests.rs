//! Wire-level ACP rate-limit contracts and their shared scripted harness.

use super::install_handlers;
use super::rate_limit_test_support::{
    PROVIDER, assert_request_floor, notice_messages, notice_values, provider_config, rate_limit_snapshots,
    rate_limited_response, run_scripted_prompt, successful_stream_response,
};
use super::tests::build_wire_test_agent_with_providers;
use crate::acp;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo, on_receive_notification};
use assert_fs::TempDir;
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, UNIX_EPOCH};
use tokio::sync::Notify;
use vtcode_commons::llm::RateLimitMetadata;
use vtcode_core::llm::provider::{LLMError, LLMErrorMetadata};

const FIXED_OBSERVED_EPOCH_SECONDS: u64 = 1_735_689_600;

#[tokio::test]
async fn baseten_headers_publish_notices_and_lody_snapshots_across_retained_retry_floor() {
    let run = run_scripted_prompt(
        provider_config(),
        vec![
            rate_limited_response(&[
                ("retry-after", "0.08"),
                ("x-ratelimit-limit-requests", "120"),
                ("x-ratelimit-remaining-requests", "30"),
                ("x-ratelimit-limit-tokens", "12000"),
                ("x-ratelimit-remaining-tokens", "3000"),
            ]),
            rate_limited_response(&[]),
            successful_stream_response(),
        ],
    )
    .await;

    assert_eq!(run.initialized_lody["rateLimits"]["version"], 1);
    assert!(run.initialized_lody["rateLimits"].get("query").is_none());
    assert_eq!(run.request_times.len(), 3);
    assert_request_floor(&run.request_times, 0, StdDuration::from_millis(70));
    assert_request_floor(&run.request_times, 1, StdDuration::from_millis(145));
    assert_eq!(run.visible_text, "recovered");

    let messages = notice_messages(&run.notifications);
    assert_eq!(messages.len(), 2);
    assert!(messages[0].contains("provider Retry-After: 0.08"));
    assert!(messages[0].contains("request limit/min: 120"));
    assert!(messages[0].contains("requests remaining/min: 30"));
    assert!(messages[0].contains("token limit/min: 12000"));
    assert!(messages[0].contains("tokens remaining/min: 3000"));
    assert!(messages[1].contains("VTCode will retry in 0.2s"));
    assert!(
        notice_values(&run.notifications)
            .iter()
            .all(|notice| notice["_meta"]["lody"].get("rateLimits").is_none()),
        "standard ACP notices must not fabricate inline Lody rateLimits metadata"
    );

    let snapshots = rate_limit_snapshots(&run.notifications);
    assert_eq!(snapshots.len(), 1, "only the response with usable quota metrics should publish a snapshot");
    let limits = snapshots[0]["rateLimits"].as_array().expect("Lody rateLimits array");
    let windows = limits
        .iter()
        .flat_map(|limit| limit["windows"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    assert_eq!(windows.len(), 2);
    assert!(windows.iter().all(|window| window["usedPercent"].as_f64() == Some(75.0)));
    assert!(windows.iter().all(|window| window["windowDurationSeconds"] == 60));
    assert!(limits.iter().all(|limit| limit["scope"]["providerId"] == PROVIDER));
}

#[tokio::test]
async fn fireworks_mappings_publish_throughput_limits_but_keep_request_counters_in_the_notice() {
    let mut config = provider_config();
    config.rate_limit_headers = vtcode_config::core::RateLimitHeaderConfig::for_provider_name("fireworks");
    let run = run_scripted_prompt(
        config,
        vec![
            rate_limited_response(&[
                ("retry-after", "0.05"),
                ("x-ratelimit-limit-tokens-prompt", "500"),
                ("x-ratelimit-limit-tokens-cache-adjusted-prompt", "450"),
                ("x-ratelimit-limit-tokens-generated", "125"),
                ("fireworks-prompt-tokens", "31"),
                ("fireworks-cached-prompt-tokens", "7"),
            ]),
            successful_stream_response(),
        ],
    )
    .await;

    let messages = notice_messages(&run.notifications);
    assert_eq!(messages.len(), 1);
    for detail in [
        "prompt token limit/s: 500",
        "cache-adjusted prompt token limit/s: 450",
        "generated token limit/s: 125",
        "request prompt tokens: 31",
        "request cached prompt tokens: 7",
    ] {
        assert!(messages[0].contains(detail), "missing Fireworks notice detail: {detail}");
    }

    let snapshots = rate_limit_snapshots(&run.notifications);
    assert_eq!(snapshots.len(), 1);
    let limits = snapshots[0]["rateLimits"].as_array().expect("Fireworks rateLimits array");
    assert_eq!(limits.len(), 3, "per-request counters must not be duplicated as quota limits");
    assert!(
        limits
            .iter()
            .all(|limit| limit["windows"].as_array().is_some_and(Vec::is_empty))
    );
    let names = limits
        .iter()
        .filter_map(|limit| limit["limitName"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    for numeric_limit in ["500", "450", "125"] {
        assert!(names.contains(numeric_limit), "missing Fireworks numeric limit {numeric_limit}: {names}");
    }
}

#[tokio::test]
async fn together_fractional_reset_sets_retry_floor_and_one_second_lody_windows() {
    let mut config = provider_config();
    config.rate_limit_headers = vtcode_config::core::RateLimitHeaderConfig::for_provider_name("together");
    let run = run_scripted_prompt(
        config,
        vec![
            rate_limited_response(&[
                ("x-ratelimit-limit", "10"),
                ("x-ratelimit-remaining", "4"),
                ("x-tokenlimit-limit", "100"),
                ("x-tokenlimit-remaining", "20"),
                ("x-ratelimit-reset", "0.075"),
            ]),
            successful_stream_response(),
        ],
    )
    .await;

    assert_eq!(run.request_times.len(), 2);
    assert_request_floor(&run.request_times, 0, StdDuration::from_millis(65));
    let messages = notice_messages(&run.notifications);
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("provider reset interval: 0.075s"));

    let snapshots = rate_limit_snapshots(&run.notifications);
    assert_eq!(snapshots.len(), 1);
    let limits = snapshots[0]["rateLimits"].as_array().expect("Together rateLimits array");
    let windows = limits
        .iter()
        .flat_map(|limit| limit["windows"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    assert_eq!(windows.len(), 2);
    assert!(windows.iter().any(|window| window["usedPercent"].as_f64() == Some(60.0)));
    assert!(windows.iter().any(|window| window["usedPercent"].as_f64() == Some(80.0)));
    assert!(windows.iter().all(|window| window["windowDurationSeconds"] == 1));
    assert_eq!(
        windows
            .iter()
            .filter(|window| window["resetsAtEpochSeconds"].is_number())
            .count(),
        1,
        "Together reset applies only to the request window"
    );
}

#[tokio::test]
async fn malformed_and_headerless_rate_limits_fall_back_to_the_capped_local_policy() {
    let run = run_scripted_prompt(
        provider_config(),
        vec![
            rate_limited_response(&[
                ("retry-after", "soon"),
                ("x-ratelimit-limit-requests", "many"),
                ("x-ratelimit-remaining-requests", "18446744073709551616"),
                ("x-ratelimit-limit-tokens", "-1"),
            ]),
            rate_limited_response(&[]),
            successful_stream_response(),
        ],
    )
    .await;

    assert_eq!(run.request_times.len(), 3);
    assert_request_floor(&run.request_times, 0, StdDuration::from_millis(40));
    assert_request_floor(&run.request_times, 1, StdDuration::from_millis(40));
    let messages = notice_messages(&run.notifications);
    assert_eq!(messages.len(), 2);
    assert!(messages[0].contains("provider Retry-After: soon"));
    assert!(messages[0].contains("VTCode will retry in 0.1s"));
    assert!(messages[1].contains("VTCode will retry in 0.1s"));
    assert!(rate_limit_snapshots(&run.notifications).is_empty());
}

#[tokio::test]
async fn fixed_epoch_rate_limit_snapshot_uses_transport_observation() {
    let workspace = TempDir::new().expect("fixed-epoch rate-limit workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[provider_config()]).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let notifications = Arc::new(Mutex::new(Vec::<acp::AgentNotification>::new()));
    let snapshot_received = Arc::new(Notify::new());

    let agent_connection = install_handlers(
        Agent.builder().name("vtcode-fixed-epoch-rate-limit-test"),
        Arc::clone(&agent),
    )
    .connect_with(agent_channel, {
        let agent = Arc::clone(&agent);
        async move |cx: ConnectionTo<Client>| {
            agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
            std::future::pending::<agent_client_protocol::Result<()>>().await
        }
    });
    let agent_task = tokio::spawn(agent_connection);

    let client_connection = Client
        .builder()
        .on_receive_notification(
            {
                let notifications = Arc::clone(&notifications);
                let snapshot_received = Arc::clone(&snapshot_received);
                async move |notification: acp::AgentNotification, _cx| {
                    let is_rate_limit_snapshot = matches!(
                        &notification,
                        acp::AgentNotification::ExtNotification(notification)
                            if notification
                                .method
                                .as_ref()
                                .trim_start_matches('_')
                                .ends_with("lody/rate_limits/update")
                    );
                    notifications.lock().expect("fixed-epoch notifications").push(notification);
                    if is_rate_limit_snapshot {
                        snapshot_received.notify_one();
                    }
                    Ok(())
                }
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, {
            let agent = Arc::clone(&agent);
            let snapshot_received = Arc::clone(&snapshot_received);
            async move |cx: ConnectionTo<Agent>| {
                drop(
                    cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                        .block_task()
                        .await?,
                );
                let session = cx.send_request(NewSessionRequest::new(workspace_path)).block_task().await?;
                let metadata = LLMErrorMetadata::new(
                    PROVIDER,
                    Some(429),
                    None,
                    None,
                    None,
                    Some("1".to_string()),
                    Some("fixture quota exhausted".to_string()),
                )
                .with_rate_limit(Some(RateLimitMetadata {
                    requests_limit_per_minute: Some(120),
                    requests_remaining_per_minute: Some(30),
                    tokens_limit_per_minute: Some(12_000),
                    tokens_remaining_per_minute: Some(3_000),
                    ..RateLimitMetadata::default()
                }));
                let error = LLMError::RateLimit { metadata: Some(metadata) };
                agent
                    .publish_rate_limit_notice_at(
                        &session.session_id,
                        PROVIDER,
                        &error,
                        None,
                        UNIX_EPOCH + StdDuration::from_secs(FIXED_OBSERVED_EPOCH_SECONDS),
                    )
                    .await;
                tokio::time::timeout(StdDuration::from_secs(2), snapshot_received.notified())
                    .await
                    .expect("fixed-epoch rate-limit snapshot deadline");
                Ok(())
            }
        });

    tokio::time::timeout(StdDuration::from_secs(3), client_connection)
        .await
        .expect("fixed-epoch ACP flow should finish")
        .expect("fixed-epoch ACP flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    let notifications = std::mem::take(&mut *notifications.lock().expect("fixed-epoch notifications"));
    let snapshots = rate_limit_snapshots(&notifications);
    assert_eq!(snapshots.len(), 1, "the fixed-epoch error should publish one Lody snapshot");
    assert_eq!(snapshots[0]["fetchedAtEpochSeconds"], FIXED_OBSERVED_EPOCH_SECONDS);
}

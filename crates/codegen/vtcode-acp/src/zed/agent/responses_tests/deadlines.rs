//! First-token, idle, and total stream deadline behaviour.
use super::super::provider::{StreamDeadlineTracker, StreamTimeoutPhase};
use super::super::tests::{
    PROMPT_PROVIDER_TEST_LOCK, PromptProviderFactoryGuard, build_wire_test_agent_with_providers,
};
use super::super::*;
use super::support::*;
use crate::acp::{InitializeRequest, NewSessionRequest, SetSessionConfigOptionRequest};
use agent_client_protocol::{Channel, on_receive_notification};
use assert_fs::TempDir;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::routing::post;
use futures::stream;
use serde_json::{Value, json};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration as StdDuration, Instant as StdInstant};
use tokio::sync::{Notify, mpsc};
use vtcode_config::core::{
    AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig, CustomProviderRequestPolicyConfig,
};
use vtcode_core::core::message_metadata::MessageMetadata;
use vtcode_core::llm::provider::{Message as ProviderMessage, MessageRole as ProviderMessageRole};
use vtcode_llm::providers::CustomProviderBackendRouter;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[derive(Clone)]
struct PacedResponsesState {
    frames: Arc<[(StdDuration, Bytes)]>,
    requests: Arc<AtomicUsize>,
}

/// Streams the configured frames with per-frame delays and counts each incoming request.
async fn paced_responses(State(state): State<PacedResponsesState>, _body: Bytes) -> Response<Body> {
    let _previous_request_count = state.requests.fetch_add(1, Ordering::SeqCst);
    let body = Body::from_stream(stream::unfold((0_usize, state.frames), |(frame_index, frames)| async move {
        let (delay, frame) = frames.get(frame_index)?.clone();
        tokio::time::sleep(delay).await;
        Some((Ok::<Bytes, std::io::Error>(frame), (frame_index + 1, frames)))
    }));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .expect("paced Responses response")
}

/// Runs one ACP prompt against a paced Responses server and returns the run plus request count.
async fn run_paced_responses(
    frames: Vec<(StdDuration, Bytes)>,
    first_token_seconds: u64,
    idle_seconds: u64,
    total_seconds: u64,
) -> (AcpRun, usize) {
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/responses", post(paced_responses))
        .with_state(PacedResponsesState {
            frames: frames.into(),
            requests: Arc::clone(&requests),
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind paced Responses server");
    let address = listener.local_addr().expect("paced Responses server address");
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve paced Responses fixture");
    });
    let mut config = provider_config(format!("http://{address}"));
    config.request_policy.max_retries = 0;
    config.request_policy.first_token_timeout_seconds = first_token_seconds;
    config.request_policy.stream_idle_timeout_seconds = idle_seconds;
    config.request_policy.total_generation_timeout_seconds = total_seconds;

    let run = run_acp_prompts(config, &["Exercise Responses stream deadlines"], ACP_PROTOCOL_DEADLINE).await;
    server_task.abort();
    drop(server_task.await);
    (run, requests.load(Ordering::SeqCst))
}

/// Builds an SSE `response.output_text.delta` frame carrying the given text.
fn responses_delta(text: &str) -> Bytes {
    Bytes::from(format!("data: {}\n\n", json!({"type":"response.output_text.delta","delta":text})))
}

/// Builds an SSE `response.completed` frame carrying the given final text.
fn responses_completed(text: &str) -> Bytes {
    Bytes::from(format!(
        "data: {}\n\n",
        json!({
            "type":"response.completed",
            "response":{
                "id":"resp_timed",
                "status":"completed",
                "model":MODEL,
                "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}],
                "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}
            }
        })
    ))
}

/// Proves output progress moves the idle deadline exactly while the total deadline stays fixed.
#[test]
fn responses_output_progress_updates_exact_idle_boundary_without_sliding_total_deadline() {
    let started_at = Instant::now();
    let total_deadline = started_at + Duration::from_secs(10);
    let mut tracker = StreamDeadlineTracker::new(
        ProviderDeadlinePolicy {
            connect: None,
            first_token: Some(Duration::from_secs(5)),
            stream_idle: Some(Duration::from_secs(3)),
            total_generation: Some(Duration::from_secs(10)),
        },
        started_at,
    );

    // Responses lifecycle/status frames do not call `observe_output_at`; only
    // model text, reasoning, or tool-input progress moves these deadlines.
    assert_eq!(tracker.next(), Some((StreamTimeoutPhase::FirstToken, started_at + Duration::from_secs(5))));
    tracker.observe_output_at(started_at + Duration::from_secs(5));
    assert_eq!(tracker.next(), Some((StreamTimeoutPhase::InterTokenIdle, started_at + Duration::from_secs(8))));
    tracker.observe_output_at(started_at + Duration::from_secs(8));
    assert_eq!(tracker.next(), Some((StreamTimeoutPhase::TotalGeneration, total_deadline)));
    tracker.observe_output_at(started_at + Duration::from_secs(9));
    assert_eq!(tracker.next(), Some((StreamTimeoutPhase::TotalGeneration, total_deadline)));
    assert_eq!(tracker.total, Some(total_deadline));
}

/// Proves a healthy output trickle keeps the idle deadline open without extending the total budget.
#[test]
fn responses_healthy_trickle_keeps_idle_open_but_still_preserves_total_budget() {
    let started_at = Instant::now();
    let total_deadline = started_at + Duration::from_secs(20);
    let mut tracker = StreamDeadlineTracker::new(
        ProviderDeadlinePolicy {
            connect: None,
            first_token: Some(Duration::from_secs(5)),
            stream_idle: Some(Duration::from_secs(4)),
            total_generation: Some(Duration::from_secs(20)),
        },
        started_at,
    );

    for elapsed_seconds in [4, 7, 10, 13, 16, 17] {
        let progress_at = started_at + Duration::from_secs(elapsed_seconds);
        tracker.observe_output_at(progress_at);
        let (_phase, next_deadline) = tracker.next().expect("healthy Responses trickle deadline");
        assert!(next_deadline > progress_at);
        assert_eq!(tracker.total, Some(total_deadline));
    }
    assert_eq!(tracker.next(), Some((StreamTimeoutPhase::TotalGeneration, total_deadline)));
}

/// Proves a slow first token fails as a bounded network error with no replay and no leaked permit.
#[tokio::test]
async fn custom_responses_enforces_first_token_deadline_without_replay_or_permit_leak() {
    let (run, requests) =
        run_paced_responses(vec![(StdDuration::from_millis(1_100), responses_delta("too late"))], 1, 3, 3).await;

    assert_eq!(requests, 1);
    assert_eq!(run.active_acp_requests, 0);
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    let visible = visible_text(&run.notifications);
    // Paced fixtures, incomplete-message checks, and tracker tests distinguish
    // phases; the client projection exposes only the bounded category.
    assert!(visible.contains("Provider failure category: Network error."));
    assert!(!visible.contains("time to first token"));
    assert!(!visible.contains("too late"));
}

/// Proves an idle timeout after partial output keeps the prefix, marks it incomplete, and does not replay.
#[tokio::test]
async fn custom_responses_enforces_idle_deadline_after_partial_output_without_replay() {
    let (run, requests) = run_paced_responses(
        vec![
            (StdDuration::from_millis(10), responses_delta("idle prefix")),
            (StdDuration::from_millis(1_100), responses_completed("idle prefix")),
        ],
        2,
        1,
        3,
    )
    .await;

    assert_eq!(requests, 1, "visible Responses output makes timeout replay unsafe");
    assert_eq!(run.active_acp_requests, 0);
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    let visible = visible_text(&run.notifications);
    assert!(visible.contains("idle prefix"));
    assert!(visible.contains("Provider failure category: Network error."));
    assert!(!visible.contains("inter-token idle"));
    assert!(run.messages.iter().any(|message| {
        message.role == ProviderMessageRole::Assistant
            && message.content.as_text().contains("idle prefix")
            && message.metadata.as_ref().is_some_and(MessageMetadata::is_incomplete)
    }));
}

/// Proves the total generation deadline is not extended by paced output and the turn ends incomplete.
#[tokio::test]
async fn custom_responses_total_deadline_is_not_extended_by_paced_output() {
    let (run, requests) = run_paced_responses(
        vec![
            (StdDuration::from_millis(100), responses_delta("one ")),
            (StdDuration::from_millis(600), responses_delta("two ")),
            (StdDuration::from_millis(600), responses_completed("one two")),
        ],
        1,
        1,
        1,
    )
    .await;

    assert_eq!(requests, 1, "total timeout after visible output must not replay Responses");
    assert_eq!(run.active_acp_requests, 0);
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    let visible = visible_text(&run.notifications);
    assert!(visible.contains("one two"));
    assert!(visible.contains("Provider failure category: Network error."));
    assert!(!visible.contains("total generation"));
    assert!(run.messages.iter().any(|message| {
        message.role == ProviderMessageRole::Assistant
            && message.content.as_text().contains("one two")
            && message.metadata.as_ref().is_some_and(MessageMetadata::is_incomplete)
    }));
}

//! Scripted-server coverage for retry and transport-selection paths.

use crate::provider::LLMProvider;
use serde_json::Value;

use super::super::WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE;
use super::support::{
    ScriptedReply, seed_continuation_cache, spawn_scripted_websocket_server, websocket_test_custom_provider,
    websocket_test_provider, websocket_test_request,
};

#[test]
fn websocket_cache_does_not_affect_normal_http_sse_payload() {
    let provider = websocket_test_provider("https://api.openai.com/v1".to_string());
    let mut request = websocket_test_request();
    request.previous_response_id = Some("resp_http_caller".to_string());
    request.response_store = Some(true);
    seed_continuation_cache(&provider, &request, "resp_cached", false);

    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("normal responses payload should build");

    assert!(payload.get("previous_response_id").is_none());
    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
}

#[tokio::test]
async fn websocket_reconnects_after_connection_limit_error() {
    let Some((base_url, recorded, handle)) = spawn_scripted_websocket_server(vec![
        vec![ScriptedReply::Error {
            code: WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE,
            message: "Responses websocket connection limit reached (60 minutes).",
        }],
        vec![
            ScriptedReply::Completed { response_id: "resp_warmup", text: "" },
            ScriptedReply::Completed { response_id: "resp_reconnected", text: "ok" },
        ],
    ])
    .await
    else {
        return;
    };
    let provider = websocket_test_provider(base_url);
    let request = websocket_test_request();
    seed_continuation_cache(&provider, &request, "resp_cached", true);

    let response = LLMProvider::generate(&provider, request)
        .await
        .expect("websocket retry should succeed");

    assert_eq!(response.content.as_deref(), Some("ok"));
    {
        let recorded = recorded.lock().expect("recorded lock");
        assert_eq!(recorded.len(), 3);
        assert_eq!(recorded[0].get("previous_response_id").and_then(Value::as_str), Some("resp_cached"));
        assert!(recorded[1].get("previous_response_id").is_none());
        assert_eq!(recorded[1].get("generate").and_then(Value::as_bool), Some(false));
        assert_eq!(recorded[2].get("previous_response_id").and_then(Value::as_str), Some("resp_warmup"));
        assert!(
            recorded
                .iter()
                .all(|payload| payload.get("store").and_then(Value::as_bool) == Some(false)),
            "websocket requests must enforce store=false"
        );
    }
    handle.await.expect("server task");
}

#[tokio::test]
async fn websocket_custom_provider_uses_compatible_transport() {
    let Some((base_url, recorded, handle)) = spawn_scripted_websocket_server(vec![vec![
        ScriptedReply::Completed { response_id: "resp_warmup", text: "" },
        ScriptedReply::Completed { response_id: "resp_final", text: "compat ok" },
    ]])
    .await
    else {
        return;
    };
    let compatible_base_url = base_url.replacen("api.openai.com", "compat.example", 1);
    let provider = websocket_test_custom_provider(compatible_base_url);

    let response = LLMProvider::generate(&provider, websocket_test_request())
        .await
        .expect("custom compatible provider should use websocket mode");

    assert_eq!(response.content.as_deref(), Some("compat ok"));
    {
        let recorded = recorded.lock().expect("recorded lock");
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].get("generate").and_then(Value::as_bool), Some(false));
        assert!(recorded[0].get("previous_response_id").is_none());
        assert_eq!(recorded[1].get("previous_response_id").and_then(Value::as_str), Some("resp_warmup"));
    }
    handle.await.expect("server task");
}

#[tokio::test]
async fn websocket_reconnects_after_active_response_error() {
    let Some((base_url, recorded, handle)) = spawn_scripted_websocket_server(vec![
        vec![ScriptedReply::Error {
            code: "invalid_request_error",
            message: "Conversation already has an active response in progress: resp_active.",
        }],
        vec![
            ScriptedReply::Completed { response_id: "resp_warmup", text: "" },
            ScriptedReply::Completed {
                response_id: "resp_after_active",
                text: "retried ok",
            },
        ],
    ])
    .await
    else {
        return;
    };
    let provider = websocket_test_provider(base_url);
    let request = websocket_test_request();
    seed_continuation_cache(&provider, &request, "resp_cached", true);

    let response = LLMProvider::generate(&provider, request)
        .await
        .expect("active-response retry should succeed");

    assert_eq!(response.content.as_deref(), Some("retried ok"));
    {
        let recorded = recorded.lock().expect("recorded lock");
        assert_eq!(recorded.len(), 3);
        assert_eq!(recorded[0].get("previous_response_id").and_then(Value::as_str), Some("resp_cached"));
        assert!(recorded[1].get("previous_response_id").is_none());
        assert_eq!(recorded[1].get("generate").and_then(Value::as_bool), Some(false));
        assert_eq!(recorded[2].get("previous_response_id").and_then(Value::as_str), Some("resp_warmup"));
    }
    handle.await.expect("server task");
}

#[tokio::test]
async fn websocket_failed_turn_drops_nonpersistent_continuation_before_retry() {
    let Some((base_url, recorded, handle)) = spawn_scripted_websocket_server(vec![
        vec![ScriptedReply::Error {
            code: "invalid_request_error",
            message: "Conversation already has an active response in progress: resp_active.",
        }],
        vec![
            ScriptedReply::Completed { response_id: "resp_warmup", text: "" },
            ScriptedReply::Completed { response_id: "resp_after_reset", text: "reset ok" },
        ],
    ])
    .await
    else {
        return;
    };
    let provider = websocket_test_provider(base_url);
    let request = websocket_test_request();
    seed_continuation_cache(&provider, &request, "resp_cached", false);

    let response = LLMProvider::generate(&provider, request)
        .await
        .expect("failed turn should restart nonpersistent continuation cleanly");

    assert_eq!(response.content.as_deref(), Some("reset ok"));
    {
        let recorded = recorded.lock().expect("recorded lock");
        assert_eq!(recorded.len(), 3);
        assert_eq!(recorded[0].get("previous_response_id").and_then(Value::as_str), Some("resp_cached"));
        assert!(recorded[1].get("previous_response_id").is_none());
        assert_eq!(recorded[1].get("generate").and_then(Value::as_bool), Some(false));
        assert_eq!(recorded[2].get("previous_response_id").and_then(Value::as_str), Some("resp_warmup"));
    }
    handle.await.expect("server task");
}

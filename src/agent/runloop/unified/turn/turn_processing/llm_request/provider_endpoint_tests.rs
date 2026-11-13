//! End-to-end coverage through a real provider adapter and a local endpoint.
//!
//! These tests build the prompt through the production assembly path, hand the
//! request to a real OpenAI-compatible provider, and inspect the bytes that
//! reach a loopback HTTP server. They complement the unit tests in `tests.rs`,
//! which record `LLMRequest` values before any wire encoding happens.

use super::*;
use crate::agent::runloop::unified::turn::turn_processing::test_support::TestTurnProcessingBacking;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// Chat-completions SSE frames the local endpoint replays to the provider.
const STREAM_BODY: &str = concat!(
    "data: {\"id\":\"chatcmpl-local\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-local\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"gpt-4o-mini\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// Serve one chat-completions request and return the captured JSON body.
///
/// Binds a loopback listener, replies with a minimal SSE stream, and sends the
/// decoded request body back through the returned receiver.
async fn serve_chat_completion_once() -> (String, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("local endpoint should bind");
    let address = listener.local_addr().expect("local endpoint address should be available");
    let (body_tx, body_rx) = oneshot::channel();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("provider should connect");
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let bytes_read = socket.read(&mut buffer).await.expect("request should be readable");
            assert_ne!(bytes_read, 0, "request should include HTTP headers");
            request.extend_from_slice(&buffer[..bytes_read]);
            if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };

        let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .expect("request should declare a content length");
        while request.len() < header_end + content_length {
            let bytes_read = socket.read(&mut buffer).await.expect("request body should be readable");
            assert_ne!(bytes_read, 0, "request body should be complete");
            request.extend_from_slice(&buffer[..bytes_read]);
        }

        let body = String::from_utf8(request[header_end..header_end + content_length].to_vec())
            .expect("request body should be UTF-8");
        let _ = body_tx.send(body);

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{STREAM_BODY}",
            STREAM_BODY.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("response should be writable");
        socket.flush().await.expect("response should flush");
        socket.shutdown().await.expect("connection should close");
    });

    (format!("http://{address}/v1"), body_rx)
}

/// Concatenate the system-role message contents from a chat-completions body.
fn system_prompt_from_body(body: &str) -> String {
    let payload: serde_json::Value = serde_json::from_str(body).expect("request body should be JSON");
    payload["messages"]
        .as_array()
        .expect("chat request should carry messages")
        .iter()
        .filter(|message| message["role"] == "system")
        .filter_map(|message| message["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn real_provider_adapter_forwards_refreshed_catalogue_metadata_to_local_endpoint() {
    let (base_url, body_rx) = serve_chat_completion_once().await;

    let base_prompt =
        "You are Diapason.\n\n[Runtime Tool Catalog]\n- version: 1\n- epoch: 2\n\n[Runtime Notice]\nKeep me";
    let mut backing = TestTurnProcessingBacking::new_with_base_prompt(4, base_prompt.to_string()).await;
    backing
        .add_tool_definition(uni::ToolDefinition::function(
            "read_file".to_string(),
            "Read a file".to_string(),
            serde_json::json!({"type": "object", "properties": {}}),
        ))
        .await;

    let provider = diapason_core::llm::OpenAIProvider::from_custom_config(
        "local-endpoint".to_string(),
        "Local Endpoint".to_string(),
        Some("test-key".to_string()),
        Some("gpt-4o-mini".to_string()),
        Some(base_url),
        None,
        None,
        None,
        None,
        None,
        None,
    );

    let mut ctx = backing.turn_processing_context();
    *ctx.provider_client = Box::new(provider);
    ctx.working_history.push(uni::Message::user("hello".to_string()));

    let result = execute_llm_request(&mut ctx, 1, "gpt-4o-mini", Some(320), false, None).await;
    assert!(result.is_ok(), "request should reach the local endpoint: {result:?}");

    let body = body_rx.await.expect("local endpoint should capture the request body");
    let system_prompt = system_prompt_from_body(&body);

    assert_eq!(
        system_prompt.matches("[Runtime Tool Catalogue]").count(),
        1,
        "the renamed header should appear exactly once: {system_prompt}"
    );
    assert!(
        !system_prompt.contains("[Runtime Tool Catalog]"),
        "the legacy header should be refreshed away: {system_prompt}"
    );
    assert!(
        system_prompt.contains("[Runtime Notice]\nKeep me"),
        "a notice adjacent to the legacy section should survive the refresh: {system_prompt}"
    );
    assert!(
        system_prompt.contains("- catalogue_tools: 1"),
        "the refreshed metadata should describe the active catalogue: {system_prompt}"
    );
    assert!(
        system_prompt.contains("- available_tools: 1"),
        "the refreshed metadata should describe the available tools: {system_prompt}"
    );
}

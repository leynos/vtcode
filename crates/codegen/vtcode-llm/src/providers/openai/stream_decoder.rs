//! Streaming decoders for OpenAI Chat Completions and Responses APIs.
//!
//! Retained custom decoder boundary: Rig's SSE parser does not currently prove
//! parity for VTCode's legacy `LLMStreamEvent` shape, fallback from empty final
//! Responses output to streamed deltas, cached prompt usage overlay, and
//! provider-specific error mapping. Protected by this module's
//! `stream_decoder` tests and provider mock streaming tests. Remove only once a
//! Rig stream adapter preserves the same final `LLMResponse` and runtime event
//! behaviour.

use super::responses_api::parse_responses_payload;
use super::streaming::OpenAIStreamTelemetry;
use crate::error_display;
use crate::provider;
use crate::providers::shared::StreamTelemetry;
use crate::providers::shared::parse_cache_write_tokens_from_usage;
use crate::providers::shared::parse_cached_prompt_tokens_from_usage;
use crate::providers::shared::{
    ResponsesStreamEventPolicy, StreamAssemblyError, extract_data_payload, response_stream_event_policy,
};
use async_stream::try_stream;
use futures::StreamExt;
use hashbrown::HashMap;
use serde_json::{Value, json};
use std::borrow::Cow;

fn streaming_network_error(provider_name: &str, error: reqwest::Error) -> provider::LLMError {
    crate::providers::error_handling::format_reqwest_network_error(provider_name, &error)
}

fn parse_stream_payload(payload: &str) -> Result<Value, provider::LLMError> {
    serde_json::from_str(payload)
        .map_err(|error| StreamAssemblyError::InvalidPayload(error.to_string()).into_llm_error("OpenAI"))
}

enum ParsedResponsesSseEvent {
    Ignore,
    Done,
    Payload(Value),
}

fn parse_responses_sse_event(event: &str) -> Result<ParsedResponsesSseEvent, provider::LLMError> {
    let Some(data_payload) = extract_data_payload(event) else {
        return Ok(ParsedResponsesSseEvent::Ignore);
    };
    let trimmed_payload = data_payload.trim();
    if trimmed_payload.is_empty() {
        return Ok(ParsedResponsesSseEvent::Ignore);
    }
    if trimmed_payload == "[DONE]" {
        return Ok(ParsedResponsesSseEvent::Done);
    }

    parse_stream_payload(trimmed_payload).map(ParsedResponsesSseEvent::Payload)
}

fn capture_chat_usage(payload: &mut Value, aggregator: &mut crate::providers::shared::StreamAggregator) {
    if payload.get("usage").is_some()
        && let Some(usage) = crate::providers::common::parse_usage_openai_format(payload, false)
    {
        aggregator.set_usage(usage);
    }
}

fn parse_chat_stream_event(
    event: &str,
    termination: &mut ChatStreamTermination,
) -> Result<Option<Value>, provider::LLMError> {
    let Some(data_payload) = extract_data_payload(event) else {
        return Ok(None);
    };
    let trimmed_payload = data_payload.trim();
    if trimmed_payload.is_empty() {
        return Ok(None);
    }
    if trimmed_payload == "[DONE]" {
        termination.observe_done_frame();
        return Ok(None);
    }

    parse_stream_payload(trimmed_payload).map(Some)
}

fn strip_reasoning(retain_reasoning: bool, mut response: provider::LLMResponse) -> provider::LLMResponse {
    if !retain_reasoning {
        response.reasoning = None;
        response.reasoning_details = None;
    }

    response
}

fn streamed_response_is_usable(response: &provider::LLMResponse) -> bool {
    response.content.as_deref().is_some_and(|content| !content.is_empty())
        || response.tool_calls.as_ref().is_some_and(|tool_calls| !tool_calls.is_empty())
        || response.reasoning.as_deref().is_some_and(|reasoning| !reasoning.is_empty())
        || response.reasoning_details.as_ref().is_some_and(|details| !details.is_empty())
}

fn final_response_output_is_empty(final_response: &Value) -> bool {
    final_response
        .get("output")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
}

fn merge_final_response_metadata(
    response: &mut provider::LLMResponse,
    final_response: &Value,
    include_cached_prompt_metrics: bool,
) {
    if let Some(usage_value) = final_response.get("usage") {
        let cached_prompt_tokens = parse_cached_prompt_tokens_from_usage(usage_value, include_cached_prompt_metrics);
        let cache_creation_tokens = parse_cache_write_tokens_from_usage(usage_value, include_cached_prompt_metrics);

        response.usage = Some(provider::Usage {
            prompt_tokens: usage_value
                .get("input_tokens")
                .or_else(|| usage_value.get("prompt_tokens"))
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0),
            completion_tokens: usage_value
                .get("output_tokens")
                .or_else(|| usage_value.get("completion_tokens"))
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0),
            reasoning_output_tokens: crate::providers::common::parse_reasoning_tokens_from_usage(usage_value),
            total_tokens: usage_value
                .get("total_tokens")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0),
            cached_prompt_tokens,
            cache_creation_tokens,
            cache_read_tokens: None,
            iterations: None,
        });
    }

    if let Some(request_id) = final_response
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| final_response.get("request_id").and_then(Value::as_str))
    {
        response.request_id = Some(request_id.to_string());
    }
}

#[derive(Default)]
struct ResponsesToolCallState {
    item_id_to_call_id: HashMap<String, String>,
    tool_call_indexes: HashMap<String, usize>,
    next_tool_call_index: usize,
    fabricated_ids: HashMap<usize, String>,
    fabricated_fallback_id: Option<String>,
}

impl ResponsesToolCallState {
    fn capture_metadata(
        &mut self,
        aggregator: &mut crate::providers::shared::StreamAggregator,
        item: &Value,
        output_index: Option<usize>,
    ) {
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            return;
        }

        let item_id = item.get("id").and_then(Value::as_str).filter(|value| !value.is_empty());
        let provider_call_id = item.get("call_id").and_then(Value::as_str).filter(|value| !value.is_empty());
        let Some(call_id) = provider_call_id.or(item_id) else {
            return;
        };
        let Some(name) = item.get("name").and_then(Value::as_str).or_else(|| {
            item.get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
        }) else {
            return;
        };

        self.capture_item_call_id_mapping(item_id, provider_call_id);

        let output_index =
            output_index.or_else(|| item_id.and_then(|item_id| self.tool_call_indexes.get(item_id).copied()));
        let index = self.resolve_tool_call_index(call_id, output_index);
        aggregator.handle_tool_calls(&[json!({
            "index": index,
            "id": call_id,
            "function": {
                "name": name,
            }
        })]);
    }

    fn handle_arguments_delta(
        &mut self,
        aggregator: &mut crate::providers::shared::StreamAggregator,
        payload: &Value,
    ) -> Result<(), provider::LLMError> {
        let delta = payload
            .get("delta")
            .and_then(Value::as_str)
            .ok_or_else(|| StreamAssemblyError::MissingField("delta").into_llm_error("OpenAI"))?;
        let item_id = payload.get("item_id").and_then(Value::as_str);
        let payload_call_id = payload.get("call_id").and_then(Value::as_str);
        self.capture_item_call_id_mapping(item_id, payload_call_id);
        let output_index = payload.get("output_index").and_then(Value::as_u64).map(|value| value as usize);
        let call_id = match self.resolve_provider_call_id(item_id, payload_call_id) {
            Some(call_id) => call_id,
            None => self.fabricated_call_id(output_index),
        };
        let index = self.resolve_tool_call_index(&call_id, output_index);

        if !delta.is_empty() {
            aggregator.handle_tool_calls(&[json!({
                "index": index,
                "id": call_id,
                "function": {
                    "arguments": delta,
                }
            })]);
        }

        Ok(())
    }

    fn capture_item_call_id_mapping(&mut self, item_id: Option<&str>, call_id: Option<&str>) {
        let Some(item_id) = item_id.filter(|value| !value.is_empty()) else {
            return;
        };
        let Some(call_id) = call_id.filter(|value| !value.is_empty()) else {
            return;
        };
        self.item_id_to_call_id.insert(item_id.to_string(), call_id.to_string());
    }

    fn resolve_provider_call_id(&self, item_id: Option<&str>, call_id: Option<&str>) -> Option<String> {
        call_id
            .filter(|value| !value.is_empty())
            .or_else(|| item_id.and_then(|value| self.item_id_to_call_id.get(value).map(String::as_str)))
            .or_else(|| item_id.filter(|value| !value.is_empty()))
            .map(ToOwned::to_owned)
    }

    /// Fabricate an id once per logical call and reuse it across deltas.
    /// Recomputing a fallback per delta fragments one id-less call into
    /// several builders, and index-based ids collide across responses.
    fn fabricated_call_id(&mut self, output_index: Option<usize>) -> String {
        match output_index {
            Some(index) => self
                .fabricated_ids
                .entry(index)
                .or_insert_with(crate::providers::shared::generate_tool_call_id)
                .clone(),
            None => self
                .fabricated_fallback_id
                .get_or_insert_with(crate::providers::shared::generate_tool_call_id)
                .clone(),
        }
    }

    fn resolve_tool_call_index(&mut self, call_id: &str, output_index: Option<usize>) -> usize {
        if let Some(index) = output_index {
            self.tool_call_indexes.insert(call_id.to_string(), index);
            self.next_tool_call_index = self.next_tool_call_index.max(index + 1);
            return index;
        }

        if let Some(index) = self.tool_call_indexes.get(call_id).copied() {
            return index;
        }

        let index = self.next_tool_call_index;
        self.tool_call_indexes.insert(call_id.to_string(), index);
        self.next_tool_call_index += 1;
        index
    }
}

pub(crate) fn create_chat_stream(
    response: reqwest::Response,
    provider_name: String,
    model: String,
    retain_reasoning: bool,
) -> provider::LLMStream {
    let stream = try_stream! {
        let mut body_stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut offset = 0usize;
        let mut aggregator = crate::providers::shared::StreamAggregator::new(model.clone());
        let mut termination = ChatStreamTermination::default();
        let telemetry = OpenAIStreamTelemetry;

        while let Some(chunk_result) = body_stream.next().await {
            let chunk = chunk_result.map_err(|error| streaming_network_error(&provider_name, error))?;

            buf.extend_from_slice(&chunk);

            while let Some((split_idx, delimiter_len)) = crate::providers::shared::find_sse_boundary_bytes(&buf, offset) {
                let event = std::str::from_utf8(&buf[offset..split_idx]).expect("valid utf-8 stream data");
                offset = split_idx + delimiter_len;

                let Some(mut payload) = parse_chat_stream_event(event, &mut termination)? else {
                    continue;
                };
                capture_chat_usage(&mut payload, &mut aggregator);

                if let Some(choice) = payload
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|choices| choices.first())
                {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                            telemetry.on_content_delta(content);
                            for event in aggregator.handle_content(content) {
                                yield event;
                            }
                        }

                        if retain_reasoning
                            && let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str())
                            && let Some(delta) = aggregator.handle_reasoning(reasoning)
                        {
                            telemetry.on_reasoning_delta(&delta);
                            yield provider::LLMStreamEvent::Reasoning { delta };
                        }

                        if let Some(tool_deltas) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                            aggregator.handle_tool_calls(tool_deltas);
                            telemetry.on_tool_call_delta();
                        }
                    }

                    if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                        aggregator.set_finish_reason(termination.observe_finish_reason(reason));
                    }
                }
            }

            // Drain the consumed prefix so `buf` stays bounded to the
            // unprocessed tail rather than growing for the entire stream.
            if offset > 0 {
                buf.drain(..offset);
                offset = 0;
            }
        }

        termination.ensure_terminal_frame()?;

        let response = aggregator.finalize();
        let response = strip_reasoning(retain_reasoning, response);
        yield provider::LLMStreamEvent::Completed { response: Box::new(response) };
    };

    Box::pin(stream)
}

#[derive(Default)]
struct ChatStreamTermination {
    terminal_frame_seen: bool,
}

impl ChatStreamTermination {
    fn observe_done_frame(&mut self) {
        self.terminal_frame_seen = true;
    }

    fn observe_finish_reason(&mut self, reason: &str) -> provider::FinishReason {
        self.terminal_frame_seen = true;
        match reason {
            "stop" => provider::FinishReason::Stop,
            "length" => provider::FinishReason::Length,
            "tool_calls" => provider::FinishReason::ToolCalls,
            "content_filter" => provider::FinishReason::ContentFilter,
            _ => provider::FinishReason::Stop,
        }
    }

    fn ensure_terminal_frame(&self) -> Result<(), provider::LLMError> {
        if self.terminal_frame_seen {
            return Ok(());
        }

        let formatted_error =
            error_display::format_llm_error("OpenAI", "Streaming error: response ended before a terminal frame");
        Err(provider::LLMError::Network { message: formatted_error, metadata: None })
    }
}

#[cfg(test)]
mod chat_stream_termination_tests {
    use super::ChatStreamTermination;
    use crate::provider;

    #[test]
    fn done_frame_completes_chat_stream() {
        let mut termination = ChatStreamTermination::default();
        termination.observe_done_frame();

        assert!(termination.ensure_terminal_frame().is_ok());
    }

    #[test]
    fn finish_reason_completes_chat_stream_and_preserves_mapping() {
        let mut termination = ChatStreamTermination::default();

        assert!(matches!(termination.observe_finish_reason("tool_calls"), provider::FinishReason::ToolCalls));
        assert!(termination.ensure_terminal_frame().is_ok());
    }

    #[test]
    fn eof_without_terminal_signal_is_a_network_error() {
        let termination = ChatStreamTermination::default();

        assert!(matches!(termination.ensure_terminal_frame(), Err(provider::LLMError::Network { .. })));
    }
}

pub(crate) fn create_responses_stream(
    response: reqwest::Response,
    provider_name: String,
    model: String,
    include_metrics: bool,
    retain_reasoning: bool,
) -> provider::LLMStream {
    let stream = try_stream! {
        let mut body_stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut offset = 0usize;
        let mut aggregator = crate::providers::shared::StreamAggregator::new(model.clone());
        let mut final_response: Option<Value> = None;
        let mut done = false;
        let mut tool_call_state = ResponsesToolCallState::default();
        #[cfg(debug_assertions)]
        let mut streamed_events_counter: usize = 0;
        let telemetry = OpenAIStreamTelemetry;

        while let Some(chunk_result) = body_stream.next().await {
            let chunk = chunk_result.map_err(|error| streaming_network_error(&provider_name, error))?;

            buf.extend_from_slice(&chunk);

            while let Some((split_idx, delimiter_len)) = crate::providers::shared::find_sse_boundary_bytes(&buf, offset) {
                let event = std::str::from_utf8(&buf[offset..split_idx]).expect("valid utf-8 stream data");
                offset = split_idx + delimiter_len;
                #[cfg(debug_assertions)]
                {
                    streamed_events_counter = streamed_events_counter.saturating_add(1);
                }

                let payload = match parse_responses_sse_event(event)? {
                    ParsedResponsesSseEvent::Ignore => continue,
                    ParsedResponsesSseEvent::Done => {
                        done = true;
                        break;
                    }
                    ParsedResponsesSseEvent::Payload(payload) => payload,
                };

                let event_policy = response_stream_event_policy(&payload)
                    .map_err(|message| {
                        StreamAssemblyError::InvalidPayload(message.to_string())
                            .into_llm_error("OpenAI")
                    })?;
                let event_type = payload
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| StreamAssemblyError::MissingField("type").into_llm_error("OpenAI"))?;

                match event_policy {
                    ResponsesStreamEventPolicy::DocumentedStatusMarkerNoop
                    | ResponsesStreamEventPolicy::DocumentedValueBearingRigGap => {
                        // Legacy `LLMStreamEvent` has no representation for
                        // provider-hosted code, provider-side MCP, partial
                        // images, streamed custom-tool input, or annotation
                        // metadata. Complete custom-tool input remains
                        // preserved by final `response.completed` replay.
                    }
                    ResponsesStreamEventPolicy::Unsupported => {
                        Err(StreamAssemblyError::InvalidPayload(format!(
                            "unsupported Responses stream event type `{event_type}`"
                        ))
                        .into_llm_error("OpenAI"))?;
                    }
                    ResponsesStreamEventPolicy::MeaningfulConversion => match event_type {
                            "response.output_text.delta" => {
                                let delta = payload
                                    .get("delta")
                                    .and_then(|value| value.as_str())
                                    .ok_or_else(|| {
                                        StreamAssemblyError::MissingField("delta")
                                            .into_llm_error("OpenAI")
                                    })?;
                                telemetry.on_content_delta(delta);

                                for event in aggregator.handle_content(delta) {
                                    yield event;
                                }
                            }
                            "response.refusal.delta" => {
                                let delta = payload
                                    .get("delta")
                                    .and_then(|value| value.as_str())
                                    .ok_or_else(|| {
                                        StreamAssemblyError::MissingField("delta")
                                            .into_llm_error("OpenAI")
                                    })?;
                                telemetry.on_content_delta(delta);
                                aggregator.content.push_str(delta);
                            }
                            "response.reasoning_text.delta"
                            | "response.reasoning_summary_text.delta"
                            | "response.reasoning_content.delta"
                            | "response.reasoning_text.done" => {
                                if let Some(event) = response_reasoning_event(event_type, &payload, ResponsesReasoningContext {
                                    retain_reasoning,
                                    aggregator: &mut aggregator,
                                    telemetry: &telemetry,
                                })? {
                                    yield event;
                                }
                            }
                            "response.output_item.added" | "response.output_item.done" => {
                                if let Some(item) = payload.get("item") {
                                    tool_call_state.capture_metadata(
                                        &mut aggregator,
                                        item,
                                        payload
                                            .get("output_index")
                                            .and_then(Value::as_u64)
                                            .map(|value| value as usize),
                                    );
                                }
                            }
                            "response.function_call_arguments.delta" => {
                                tool_call_state.handle_arguments_delta(&mut aggregator, &payload)?;
                                telemetry.on_tool_call_delta();
                            }
                            "response.completed" => {
                                if let Some(response_value) = payload.get("response") {
                                    final_response = Some(response_value.clone());
                                }
                                done = true;
                            }
                            "response.failed" | "response.incomplete" => {
                                let error_message = if let Some(err) = payload.get("response")
                                    .and_then(|r| r.get("error"))
                                {
                                    err.get("message")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("Unknown error")
                                } else {
                                    "Unknown error from Responses API"
                                };
                                let formatted_error = error_display::format_llm_error("OpenAI", error_message);
                                Err(provider::LLMError::Provider {
                                    message: formatted_error,
                                    metadata: None,
                                })?;
                            }
                            "error" => {
                                let error_message = payload
                                    .get("error")
                                    .and_then(|error| error.get("message"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("Unknown error from Responses API");
                                let formatted_error = error_display::format_llm_error("OpenAI", error_message);
                                Err(provider::LLMError::Provider {
                                    message: formatted_error,
                                    metadata: None,
                                })?;
                            }
                            _ => {
                                Err(StreamAssemblyError::InvalidPayload(format!(
                                    "unhandled Responses stream event type `{event_type}`"
                                ))
                                .into_llm_error("OpenAI"))?;
                            }
                    },
                }

                if done {
                    break;
                }
            }

            // Drain the consumed prefix so `buf` stays bounded to the
            // unprocessed tail rather than growing for the entire stream.
            if offset > 0 {
                buf.drain(..offset);
                offset = 0;
            }

            if done {
                break;
            }
        }

        let response_value = match final_response {
            Some(value) => value,
            None => {
                let formatted_error = error_display::format_llm_error(
                    "OpenAI",
                    "Stream ended without a completion event",
                );
                Err(provider::LLMError::Provider { message: formatted_error, metadata: None })?
            }
        };

        let final_aggregator_response = aggregator.finalize();
        let mut response = match parse_responses_payload(response_value.clone(), model.clone(), include_metrics) {
            Ok(response) => response,
            Err(_)
                if final_response_output_is_empty(&response_value)
                    && streamed_response_is_usable(&final_aggregator_response) =>
            {
                let mut response = final_aggregator_response.clone();
                merge_final_response_metadata(&mut response, &response_value, include_metrics);
                response
            }
            Err(err) => Err(err)?,
        };

        if response.content.is_none() {
            response.content = final_aggregator_response.content;
        } else if let (Some(c), Some(agg_c)) = (&mut response.content, final_aggregator_response.content)
            && !c.contains(&agg_c) {
                c.push_str(&agg_c);
            }

        if response.reasoning.is_none() {
            response.reasoning = final_aggregator_response.reasoning;
        }

        if response.tool_calls.is_none() {
            response.tool_calls = final_aggregator_response.tool_calls;
        }

        let response = strip_reasoning(retain_reasoning, response);
        yield provider::LLMStreamEvent::Completed { response: Box::new(response) };
    };

    Box::pin(stream)
}

fn optional_string_field(payload: &Value, field: &'static str) -> Result<Option<String>, provider::LLMError> {
    match payload.get(field) {
        Some(value) => value.as_str().map(|value| Some(value.to_string())).ok_or_else(|| {
            StreamAssemblyError::InvalidPayload(format!("field `{field}` in stream payload must be a string"))
                .into_llm_error("OpenAI")
        }),
        None => Ok(None),
    }
}

#[cfg(test)]
mod stream_transport_error_tests {
    use super::{create_chat_stream, create_responses_stream};
    use crate::provider::{self, LLMError};
    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const CUSTOM_PROVIDER: &str = "Example Gateway";

    async fn response_with_truncated_body(event: &'static str) -> reqwest::Response {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("test server should bind");
        let address = listener.local_addr().expect("test server address should be available");

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("test request should connect");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let bytes_read = socket.read(&mut buffer).await.expect("request should be readable");
                assert_ne!(bytes_read, 0, "request should include HTTP headers");
                request.extend_from_slice(&buffer[..bytes_read]);
            }

            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("response headers should be writable");
            socket.write_all(event.as_bytes()).await.expect("event should be writable");
            socket.flush().await.expect("event should flush");
            socket.shutdown().await.expect("connection should close after a truncated body");
        });

        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("test HTTP client should build")
            .get(format!("http://{address}/stream"))
            .send()
            .await
            .expect("headers should arrive before the truncated body")
    }

    async fn assert_custom_provider_body_error(stream: provider::LLMStream) {
        let events = stream.collect::<Vec<_>>().await;
        assert!(
            events
                .iter()
                .any(|event| { matches!(event, Ok(provider::LLMStreamEvent::Token { delta }) if delta == "partial") })
        );

        let error = events
            .into_iter()
            .find_map(Result::err)
            .expect("truncated response body should fail the stream");
        match error {
            LLMError::Network { message, metadata } => {
                let metadata = metadata.expect("reqwest transport error should include metadata");
                assert_eq!(metadata.code.as_deref(), Some("reqwest_decode_error"));
                let serialized = serde_json::to_value(metadata.as_ref()).expect("metadata should serialize");
                assert_eq!(serialized["provider"], CUSTOM_PROVIDER);
                assert!(message.contains(CUSTOM_PROVIDER), "display label missing from {message}");
            }
            other => panic!("expected a network error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_stream_classifies_post_header_disconnect_for_custom_provider() {
        let response =
            response_with_truncated_body("data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n").await;
        let stream = create_chat_stream(response, CUSTOM_PROVIDER.to_owned(), "custom-model".to_owned(), false);

        assert_custom_provider_body_error(stream).await;
    }

    #[tokio::test]
    async fn responses_stream_classifies_post_header_disconnect_for_custom_provider() {
        let response =
            response_with_truncated_body("data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n")
                .await;
        let stream =
            create_responses_stream(response, CUSTOM_PROVIDER.to_owned(), "custom-model".to_owned(), false, false);

        assert_custom_provider_body_error(stream).await;
    }
}

struct ResponsesReasoningContext<'a> {
    retain_reasoning: bool,
    aggregator: &'a mut crate::providers::shared::StreamAggregator,
    telemetry: &'a OpenAIStreamTelemetry,
}

fn response_reasoning_event(
    event_type: &str,
    payload: &Value,
    context: ResponsesReasoningContext<'_>,
) -> Result<Option<provider::LLMStreamEvent>, provider::LLMError> {
    let reasoning: Option<Cow<'_, str>> = match event_type {
        "response.reasoning_text.delta"
        | "response.reasoning_summary_text.delta"
        | "response.reasoning_content.delta" => Some(Cow::Borrowed(
            payload
                .get("delta")
                .and_then(Value::as_str)
                .ok_or_else(|| StreamAssemblyError::MissingField("delta").into_llm_error("OpenAI"))?,
        )),
        "response.reasoning_text.done" => optional_string_field(payload, "text")?
            .or(optional_string_field(payload, "delta")?)
            .map(Cow::Owned),
        _ => return Ok(None),
    };
    if !context.retain_reasoning {
        return Ok(None);
    }
    let Some(reasoning) = reasoning else {
        return Ok(None);
    };
    let Some(delta) = context.aggregator.handle_reasoning(reasoning.as_ref()) else {
        return Ok(None);
    };

    context.telemetry.on_reasoning_delta(&delta);
    Ok(Some(provider::LLMStreamEvent::Reasoning { delta }))
}

#[cfg(test)]
mod tests {
    use super::{
        ResponsesToolCallState, final_response_output_is_empty, merge_final_response_metadata,
        streamed_response_is_usable,
    };
    use crate::provider::{LLMResponse, ToolCall};
    use crate::providers::shared::StreamAggregator;
    use serde_json::json;

    #[test]
    fn responses_final_metadata_parses_cached_prompt_tokens_when_enabled() {
        let mut response = LLMResponse::default();
        merge_final_response_metadata(
            &mut response,
            &json!({
                "id": "resp_stream",
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 5,
                    "total_tokens": 17,
                    "input_tokens_details": {
                        "cached_tokens": 9
                    }
                }
            }),
            true,
        );

        assert_eq!(response.request_id.as_deref(), Some("resp_stream"));
        let usage = response.usage.expect("usage should be populated");
        assert_eq!(usage.prompt_tokens, 12);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 17);
        assert_eq!(usage.cached_prompt_tokens, Some(9));
    }

    #[test]
    fn responses_final_metadata_preserves_reasoning_tokens() {
        let mut response = LLMResponse::default();
        merge_final_response_metadata(
            &mut response,
            &json!({
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 5,
                    "total_tokens": 17,
                    "output_tokens_details": {"reasoning_tokens": 4}
                }
            }),
            false,
        );

        assert_eq!(response.usage.and_then(|usage| usage.reasoning_output_tokens), Some(4));
    }

    #[test]
    fn empty_final_response_can_use_streamed_tool_call_delta() {
        let response = LLMResponse {
            tool_calls: Some(vec![ToolCall::function(
                "call_1".to_string(),
                "search_workspace".to_string(),
                "{\"query\":\"vtcode\"}".to_string(),
            )]),
            ..Default::default()
        };

        assert!(final_response_output_is_empty(&json!({"output": []})));
        assert!(streamed_response_is_usable(&response));
    }

    #[test]
    fn responses_tool_call_state_uses_provider_call_id_when_item_id_differs() {
        let mut aggregator = StreamAggregator::new("gpt-5".to_string());
        let mut tool_call_state = ResponsesToolCallState::default();

        tool_call_state.capture_metadata(
            &mut aggregator,
            &json!({
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "search_workspace"
            }),
            Some(0),
        );
        tool_call_state
            .handle_arguments_delta(
                &mut aggregator,
                &json!({
                    "type": "response.function_call_arguments.delta",
                    "item_id": "fc_1",
                    "delta": "{\"query\":\"vtcode\"}"
                }),
            )
            .expect("tool delta should parse");

        let response = aggregator.finalize();
        assert_eq!(
            response.tool_calls.as_ref(),
            Some(&vec![ToolCall::function(
                "call_1".to_string(),
                "search_workspace".to_string(),
                "{\"query\":\"vtcode\"}".to_string(),
            )])
        );
    }

    #[test]
    fn responses_tool_call_state_reuses_fabricated_id_across_deltas() {
        let mut aggregator = StreamAggregator::new("gpt-5".to_string());
        let mut tool_call_state = ResponsesToolCallState::default();

        let delta = |fragment: &str| {
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "delta": fragment
            })
        };
        tool_call_state
            .handle_arguments_delta(&mut aggregator, &delta("{\"query\":"))
            .expect("tool delta should parse");
        tool_call_state
            .handle_arguments_delta(&mut aggregator, &delta("\"vtcode\"}"))
            .expect("tool delta should parse");

        // Supply the name the way metadata would, without an id, so finalize
        // keeps the builder created by the id-less deltas.
        aggregator.handle_tool_calls(&[json!({
            "index": 0,
            "function": {"name": "search_workspace"}
        })]);

        let calls = aggregator.finalize().tool_calls.expect("tool call expected");
        assert_eq!(calls.len(), 1, "both deltas must land on one builder");
        assert!(calls[0].id.starts_with("call_"));
        assert_ne!(calls[0].id, "call_0");
        let function = calls[0].function.as_ref().expect("function expected");
        assert_eq!(function.arguments, "{\"query\":\"vtcode\"}");
    }

    #[test]
    fn responses_tool_call_state_fabricates_distinct_ids_per_decoder() {
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut aggregator = StreamAggregator::new("gpt-5".to_string());
            let mut tool_call_state = ResponsesToolCallState::default();
            tool_call_state
                .handle_arguments_delta(
                    &mut aggregator,
                    &json!({
                        "type": "response.function_call_arguments.delta",
                        "output_index": 0,
                        "delta": "{}"
                    }),
                )
                .expect("tool delta should parse");
            aggregator.handle_tool_calls(&[json!({
                "index": 0,
                "function": {"name": "search_workspace"}
            })]);
            let calls = aggregator.finalize().tool_calls.expect("tool call expected");
            ids.push(calls[0].id.clone());
        }
        assert_ne!(ids[0], ids[1], "fabricated ids must differ across responses");
    }
}

//! Streaming decoders for OpenAI Chat Completions and Responses APIs.
//!
//! Retained custom decoder boundary: Rig's SSE parser does not currently prove
//! parity for VTCode's legacy `LLMStreamEvent` shape, fallback from empty final
//! Responses output to streamed deltas, cached prompt usage overlay, and
//! provider-specific error mapping. Protected by this module's
//! `stream_decoder` tests and provider mock streaming tests. Remove only once a
//! Rig stream adapter preserves the same final `LLMResponse` and runtime event
//! behaviour.

use crate::error_display;
use crate::provider;
use crate::providers::shared::StreamTelemetry;
use crate::providers::shared::responses_reconciler::{
    FinalInputPreference, ResponsesItemIdentity, ResponsesStreamReconciler, ResponsesTerminalState,
    reconcile_final_input,
};
use crate::providers::shared::responses_usage;
use crate::providers::shared::responses_validation;
use crate::providers::shared::responses_wire::ResponsesSseDecoder;
use crate::providers::shared::{
    ResponsesStreamEventPolicy, StreamAssemblyError, extract_data_payload, response_stream_event_policy,
};
use async_stream::try_stream;
use futures::StreamExt;
use hashbrown::HashMap;
use serde_json::{Value, json};
use std::time::Instant;

use super::responses_api::parse_responses_payload;
use super::streaming::OpenAIStreamTelemetry;

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
) -> Result<(), provider::LLMError> {
    if let Some(usage) = responses_usage::parse_usage(final_response, include_cached_prompt_metrics)? {
        response.usage = Some(usage);
    }

    if let Some(request_id) = final_response
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| final_response.get("request_id").and_then(Value::as_str))
    {
        response.request_id = Some(request_id.to_string());
    }
    Ok(())
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
    ) -> Result<Option<(String, String)>, provider::LLMError> {
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

        let progress = if delta.is_empty() {
            None
        } else {
            aggregator.handle_tool_calls(&[json!({
                "index": index,
                "id": call_id,
                "function": {
                    "arguments": delta,
                }
            })]);
            Some((call_id, delta.to_string()))
        };

        Ok(progress)
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
    model: String,
    retain_reasoning: bool,
) -> provider::LLMStream {
    let stream = try_stream! {
        let mut body_stream = response.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut offset = 0usize;
        let mut aggregator = crate::providers::shared::StreamAggregator::new(model.clone());
        let mut saw_terminal_frame = false;
        let telemetry = OpenAIStreamTelemetry;

        while let Some(chunk_result) = body_stream.next().await {
            let chunk = chunk_result.map_err(|err| {
                let formatted_error = error_display::format_llm_error(
                    "OpenAI",
                    &format!("Streaming error: {err}"),
                );
                provider::LLMError::Network { message: formatted_error, metadata: None }
            })?;

            buf.extend_from_slice(&chunk);

            while let Some((split_idx, delimiter_len)) = crate::providers::shared::find_sse_boundary_bytes(&buf, offset) {
                let event = std::str::from_utf8(&buf[offset..split_idx]).expect("valid utf-8 stream data");
                offset = split_idx + delimiter_len;

                if let Some(data_payload) = extract_data_payload(event) {
                    let trimmed_payload = data_payload.trim();
                    if trimmed_payload.is_empty() {
                        continue;
                    }
                    if trimmed_payload == "[DONE]" {
                        saw_terminal_frame = true;
                        continue;
                    }

                    let payload: Value = serde_json::from_str(trimmed_payload).map_err(|err| {
                        StreamAssemblyError::InvalidPayload(err.to_string())
                            .into_llm_error("OpenAI")
                    })?;

                    if payload.get("usage").is_some()
                        && let Some(usage) = crate::providers::common::parse_usage_openai_format(&payload, false)
                    {
                        aggregator.set_usage(usage);
                    }

                    if let Some(choices) = payload.get("choices").and_then(|v| v.as_array())
                        && let Some(choice) = choices.first() {
                            if let Some(delta) = choice.get("delta") {
                                if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                                    telemetry.on_content_delta(content);
                                    for event in aggregator.handle_content(content) {
                                        yield event;
                                    }
                                }

                                if retain_reasoning
                                    && let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str())
                                    && let Some(delta) = aggregator.handle_reasoning(reasoning) {
                                        telemetry.on_reasoning_delta(&delta);
                                        yield provider::LLMStreamEvent::Reasoning { delta };
                                    }

                                if let Some(tool_deltas) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                                    aggregator.handle_tool_calls(tool_deltas);
                                    telemetry.on_tool_call_delta();
                                }
                            }

                            if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                                saw_terminal_frame = true;
                                aggregator.set_finish_reason(match reason {
                                    "stop" => provider::FinishReason::Stop,
                                    "length" => provider::FinishReason::Length,
                                    "tool_calls" => provider::FinishReason::ToolCalls,
                                    "content_filter" => provider::FinishReason::ContentFilter,
                                    _ => provider::FinishReason::Stop,
                                });
                            }
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

        if !saw_terminal_frame {
            let formatted_error = error_display::format_llm_error(
                "OpenAI",
                "Streaming error: response ended before a terminal frame",
            );
            Err(provider::LLMError::Network { message: formatted_error, metadata: None })?;
        }

        let response = aggregator.finalize();
        let response = strip_reasoning(retain_reasoning, response);
        yield provider::LLMStreamEvent::Completed { response: Box::new(response) };
    };

    Box::pin(stream)
}

pub(crate) fn create_responses_stream(
    response: reqwest::Response,
    model: String,
    include_metrics: bool,
    _debug_model: Option<String>,
    _request_timer: Option<Instant>,
    retain_reasoning: bool,
) -> provider::LLMStream {
    let stream = try_stream! {
        let mut body_stream = response.bytes_stream();
        let mut decoder = ResponsesSseDecoder::default();
        let mut aggregator = crate::providers::shared::StreamAggregator::new(model.clone());
        let mut final_response: Option<Value> = None;
        let mut tool_call_state = ResponsesToolCallState::default();
        let mut reconciler = ResponsesStreamReconciler::default();
        let telemetry = OpenAIStreamTelemetry;

        while let Some(chunk_result) = body_stream.next().await {
            let chunk = chunk_result.map_err(|err| {
                let formatted_error = error_display::format_llm_error(
                    "OpenAI",
                    &format!("Streaming error: {err}"),
                );
                provider::LLMError::Network { message: formatted_error, metadata: None }
            })?;

            let data_payloads = decoder.push(&chunk);

            for data_payload in data_payloads {
                let data_payload = data_payload.map_err(|error| error.into_llm_error("OpenAI"))?;
                let trimmed_payload = data_payload.trim();
                if trimmed_payload.is_empty() || trimmed_payload == "[DONE]" {
                    continue;
                }

                    let payload: Value = serde_json::from_str(trimmed_payload).map_err(|err| {
                        StreamAssemblyError::InvalidPayload(err.to_string())
                            .into_llm_error("OpenAI")
                    })?;
                    let sequence_number = payload.get("sequence_number").and_then(Value::as_u64);
                    if !reconciler.admit(sequence_number) {
                        continue;
                    }

                    let event_policy = response_stream_event_policy(&payload)
                        .map_err(|message| {
                            StreamAssemblyError::InvalidPayload(message.to_string())
                                .into_llm_error("OpenAI")
                        })?;
                    let event_type = payload
                        .get("type")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            StreamAssemblyError::MissingField("type").into_llm_error("OpenAI")
                        })?;

                    match event_policy {
                        ResponsesStreamEventPolicy::DocumentedStatusMarkerNoop => {
                            // Legacy `LLMStreamEvent` has no representation for
                            // provider-hosted code, provider-side MCP, partial
                            // images, or annotation metadata.
                        }
                        ResponsesStreamEventPolicy::DocumentedValueBearingRigGap => match event_type {
                            "response.custom_tool_call_input.delta" => {
                                let identity = responses_item_identity(&payload);
                                let call_id = identity
                                    .response_call_id()
                                    .map(ToOwned::to_owned)
                                    .ok_or_else(|| StreamAssemblyError::InvalidPayload("custom tool input has no call id".to_string()).into_llm_error("OpenAI"))?;
                                let delta = required_string_field(&payload, "delta")?;
                                let delta = reconciler
                                    .custom_input_delta(identity, delta)
                                    .map_err(|message| StreamAssemblyError::InvalidPayload(message.to_string()).into_llm_error("OpenAI"))?;
                                if !delta.is_empty() {
                                    telemetry.on_tool_call_delta();
                                    yield provider::LLMStreamEvent::ToolCallDelta { call_id, delta };
                                }
                            }
                            "response.custom_tool_call_input.done" => {
                                let identity = responses_item_identity(&payload);
                                let call_id = identity
                                    .response_call_id()
                                    .map(ToOwned::to_owned)
                                    .ok_or_else(|| StreamAssemblyError::InvalidPayload("custom tool input has no call id".to_string()).into_llm_error("OpenAI"))?;
                                let input = optional_string_field(&payload, "input")?
                                    .or(optional_string_field(&payload, "delta")?)
                                    .unwrap_or_default();
                                let delta = reconciler
                                    .custom_input_done(identity, &input)
                                    .map_err(|message| StreamAssemblyError::InvalidPayload(message.to_string()).into_llm_error("OpenAI"))?;
                                if let Some(delta) = delta
                                    && !delta.is_empty()
                                {
                                    telemetry.on_tool_call_delta();
                                    yield provider::LLMStreamEvent::ToolCallDelta { call_id, delta };
                                }
                            }
                            _ => {}
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
                            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                                let delta = payload
                                    .get("delta")
                                    .and_then(|value| value.as_str())
                                    .ok_or_else(|| {
                                        StreamAssemblyError::MissingField("delta")
                                            .into_llm_error("OpenAI")
                                    })?;
                                let delta = reconciler.reasoning_delta(responses_item_identity(&payload), delta);
                                if retain_reasoning && !delta.is_empty() {
                                    aggregator.reasoning.push_str(&delta);
                                    telemetry.on_reasoning_delta(&delta);
                                    yield provider::LLMStreamEvent::Reasoning { delta };
                                }
                            }
                            "response.reasoning_content.delta" => {
                                let delta = payload
                                    .get("delta")
                                    .and_then(|value| value.as_str())
                                    .ok_or_else(|| {
                                        StreamAssemblyError::MissingField("delta")
                                            .into_llm_error("OpenAI")
                                    })?;
                                let delta = reconciler.reasoning_delta(responses_item_identity(&payload), delta);
                                if retain_reasoning && !delta.is_empty() {
                                    aggregator.reasoning.push_str(&delta);
                                    telemetry.on_reasoning_delta(&delta);
                                    yield provider::LLMStreamEvent::Reasoning { delta };
                                }
                            }
                            "response.reasoning_text.done" | "response.reasoning_summary_text.done" => {
                                let text = optional_string_field(&payload, "text")?;
                                let delta = optional_string_field(&payload, "delta")?;
                                if let Some(text) = text.or(delta) {
                                    let delta = reconciler
                                        .reasoning_done(responses_item_identity(&payload), &text)
                                        .map_err(|message| StreamAssemblyError::InvalidPayload(message.to_string()).into_llm_error("OpenAI"))?;
                                    if retain_reasoning
                                        && let Some(delta) = delta
                                        && !delta.is_empty()
                                    {
                                        aggregator.reasoning.push_str(&delta);
                                        telemetry.on_reasoning_delta(&delta);
                                        yield provider::LLMStreamEvent::Reasoning { delta };
                                    }
                                }
                            }
                            "response.output_item.added" | "response.output_item.done" => {
                                if let Some(item) = payload.get("item") {
                                    if item.get("type").and_then(Value::as_str) == Some("custom_tool_call") {
                                        let identity = responses_item_identity_from_item(&payload, item);
                                        let progress_call_id = identity.response_call_id().map(ToOwned::to_owned);
                                        let name = item.get("name").and_then(Value::as_str).map(ToOwned::to_owned);
                                        reconciler
                                            .capture_custom_call(
                                                identity,
                                                name.as_deref(),
                                                item.get("input").and_then(Value::as_str),
                                            )
                                            .map_err(|message| StreamAssemblyError::InvalidPayload(message.to_string()).into_llm_error("OpenAI"))?;
                                        if let Some(call_id) = progress_call_id {
                                            yield provider::LLMStreamEvent::ToolCallStart { call_id, name };
                                        }
                                    } else if item.get("type").and_then(Value::as_str) == Some("function_call") {
                                        let identity = responses_item_identity_from_item(&payload, item);
                                        if let Some(call_id) = identity.response_call_id().map(ToOwned::to_owned) {
                                            let name = item.get("name").and_then(Value::as_str).map(ToOwned::to_owned);
                                            yield provider::LLMStreamEvent::ToolCallStart { call_id, name };
                                        }
                                    }
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
                                if let Some((call_id, delta)) =
                                    tool_call_state.handle_arguments_delta(&mut aggregator, &payload)?
                                {
                                    telemetry.on_tool_call_delta();
                                    yield provider::LLMStreamEvent::ToolCallDelta { call_id, delta };
                                }
                            }
                            "response.completed" => {
                                if let Some(response_value) = payload.get("response") {
                                    responses_validation::validate_completed_response(response_value)?;
                                    final_response = Some(response_value.clone());
                                }
                                reconciler.mark_completed();
                            }
                            "response.failed" | "response.incomplete" => {
                                reconciler.mark_failed();
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
                                reconciler.mark_failed();
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
                if reconciler.terminal_state() != ResponsesTerminalState::Active {
                    break;
                }
            }

            if reconciler.terminal_state() != ResponsesTerminalState::Active {
                break;
            }
        }

        if reconciler.terminal_state() == ResponsesTerminalState::Active {
            decoder.finish().map_err(|error| error.into_llm_error("OpenAI"))?;
        }

        reconciler.require_completed().map_err(|message| {
            let message = error_display::format_llm_error("OpenAI", message);
            provider::LLMError::Provider { message, metadata: None }
        })?;
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

        let mut final_aggregator_response = aggregator.finalize();
        merge_reconciled_custom_calls(&mut final_aggregator_response, &reconciler)?;
        let mut response = match parse_responses_payload(response_value.clone(), model.clone(), include_metrics) {
            Ok(response) => response,
            Err(_)
                if final_response_output_is_empty(&response_value)
                    && streamed_response_is_usable(&final_aggregator_response) =>
            {
                final_aggregator_response.clone()
            }
            Err(err) => Err(err)?,
        };
        merge_final_response_metadata(&mut response, &response_value, include_metrics)?;

        if response.content.is_none() {
            response.content = final_aggregator_response.content;
        } else if let (Some(c), Some(agg_c)) = (&mut response.content, final_aggregator_response.content)
            && !c.contains(&agg_c) {
                c.push_str(&agg_c);
            }

        if response.reasoning.is_none() {
            response.reasoning = final_aggregator_response.reasoning;
        }

        reconcile_streamed_tool_calls(&mut response, final_aggregator_response.tool_calls.as_deref())?;

        let response = strip_reasoning(retain_reasoning, response);
        yield provider::LLMStreamEvent::Completed { response: Box::new(response) };
    };

    Box::pin(stream)
}

fn responses_item_identity(payload: &Value) -> ResponsesItemIdentity {
    ResponsesItemIdentity::new(
        payload.get("item_id").and_then(Value::as_str).map(ToOwned::to_owned),
        payload.get("call_id").and_then(Value::as_str).map(ToOwned::to_owned),
        payload
            .get("output_index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
    )
    .with_sub_index(
        payload
            .get("content_index")
            .or_else(|| payload.get("summary_index"))
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
    )
}

fn merge_reconciled_custom_calls(
    response: &mut provider::LLMResponse,
    reconciler: &ResponsesStreamReconciler,
) -> Result<(), provider::LLMError> {
    let streamed_calls = reconciler.custom_tool_calls();
    if streamed_calls.is_empty() {
        return Ok(());
    }

    let response_calls = response.tool_calls.get_or_insert_default();
    for streamed_call in streamed_calls {
        let tool_call = provider::ToolCall::custom(streamed_call.call_id, streamed_call.name, streamed_call.input);
        if let Some(existing) = response_calls.iter_mut().find(|call| call.id == tool_call.id) {
            reconcile_tool_call(existing, &tool_call)?;
        } else {
            response_calls.push(tool_call);
        }
    }
    Ok(())
}

fn reconcile_streamed_tool_calls(
    response: &mut provider::LLMResponse,
    streamed_calls: Option<&[provider::ToolCall]>,
) -> Result<(), provider::LLMError> {
    let Some(streamed_calls) = streamed_calls.filter(|calls| !calls.is_empty()) else {
        return Ok(());
    };
    let Some(response_calls) = response.tool_calls.as_mut() else {
        response.tool_calls = Some(streamed_calls.to_vec());
        return Ok(());
    };

    for streamed_call in streamed_calls {
        let existing = response_calls
            .iter_mut()
            .find(|call| call.id == streamed_call.id)
            .ok_or_else(|| {
                StreamAssemblyError::InvalidPayload("completed response omitted a streamed tool call".to_string())
                    .into_llm_error("OpenAI")
            })?;
        reconcile_tool_call(existing, streamed_call)?;
    }
    Ok(())
}

fn reconcile_tool_call(
    final_call: &mut provider::ToolCall,
    streamed_call: &provider::ToolCall,
) -> Result<(), provider::LLMError> {
    if final_call.call_type != streamed_call.call_type || final_call.tool_name() != streamed_call.tool_name() {
        return Err(StreamAssemblyError::InvalidPayload(
            "completed tool metadata contradicts streamed metadata".to_string(),
        )
        .into_llm_error("OpenAI"));
    }
    match reconcile_final_input(
        streamed_call.raw_input().unwrap_or_default(),
        final_call.raw_input().unwrap_or_default(),
    )
    .map_err(|message| StreamAssemblyError::InvalidPayload(message.to_string()).into_llm_error("OpenAI"))?
    {
        FinalInputPreference::Final => {}
        FinalInputPreference::Streamed => *final_call = streamed_call.clone(),
    }
    Ok(())
}

fn responses_item_identity_from_item(payload: &Value, item: &Value) -> ResponsesItemIdentity {
    ResponsesItemIdentity::new(
        payload
            .get("item_id")
            .and_then(Value::as_str)
            .or_else(|| item.get("id").and_then(Value::as_str))
            .map(ToOwned::to_owned),
        payload
            .get("call_id")
            .and_then(Value::as_str)
            .or_else(|| item.get("call_id").and_then(Value::as_str))
            .map(ToOwned::to_owned),
        payload
            .get("output_index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
    )
}

fn required_string_field<'a>(payload: &'a Value, field: &'static str) -> Result<&'a str, provider::LLMError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| StreamAssemblyError::MissingField(field).into_llm_error("OpenAI"))
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
        )
        .expect("valid usage");

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
        )
        .expect("valid usage");

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
        let _ = tool_call_state
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
        let _ = tool_call_state
            .handle_arguments_delta(&mut aggregator, &delta("{\"query\":"))
            .expect("tool delta should parse");
        let _ = tool_call_state
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
            let _ = tool_call_state
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

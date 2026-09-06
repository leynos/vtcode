use crate::error_display;
use crate::provider::{LLMError, LLMNormalizedStream, LLMResponse, NormalizedStreamEvent, ToolCall};
use crate::providers::shared::responses_adapter::{ResponsesStreamAdapter, ResponsesStreamEvent};
use crate::providers::shared::responses_reconciler::{
    FinalInputPreference, ResponsesItemIdentity, ResponsesStreamReconciler, ResponsesTerminalState,
    reconcile_final_input,
};
use crate::providers::shared::responses_usage;
use crate::providers::shared::responses_validation;
use crate::providers::shared::responses_wire::ResponsesSseDecoder;
use async_stream::try_stream;
use futures::StreamExt;
use hashbrown::{HashMap, HashSet};
use serde_json::{Value, json};

use super::correlate_streamed_function_calls;
use super::{StreamAggregator, generate_tool_call_id};

// Retained shared Responses stream processor.
// Rig 0.40 can consume SSE, but VTCode needs a provider-agnostic
// NormalizedStreamEvent contract: text/refusal/reasoning deltas, tool-call
// start and argument deltas, tolerant empty-final-response recovery, and
// backend error text. Protected by this module's `responses_stream` tests.
// Remove only when Rig exposes an event adapter with the same normalised
// surface for all VTCode providers that use Responses-style streaming.
pub struct ResponsesNormalizedStreamOptions {
    pub(crate) provider_name: &'static str,
    pub(crate) model: String,
    pub(crate) emit_reasoning: bool,
    pub(crate) include_cached_prompt_metrics: bool,
    pub(crate) allow_function_call_id_remap: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResponsesStreamEventPolicy {
    MeaningfulConversion,
    DocumentedStatusMarkerNoop,
    DocumentedValueBearingRigGap,
    Unsupported,
}

pub(crate) fn response_stream_event_policy(payload: &Value) -> Result<ResponsesStreamEventPolicy, &'static str> {
    let Some(event_type) = payload.get("type") else {
        return Err("missing Responses stream event type");
    };
    let Some(event_type) = event_type.as_str() else {
        return Err("Responses stream event type must be a string");
    };

    Ok(response_stream_event_policy_for_type(event_type))
}

fn response_stream_event_policy_for_type(event_type: &str) -> ResponsesStreamEventPolicy {
    match event_type {
        "response.output_text.delta"
        | "response.refusal.delta"
        | "response.reasoning_text.delta"
        | "response.reasoning_summary_text.delta"
        | "response.reasoning_content.delta"
        | "response.reasoning_text.done"
        | "response.reasoning_summary_text.done"
        | "response.reasoning_part.added"
        | "response.reasoning_part.done"
        | "response.output_item.added"
        | "response.output_item.done"
        | "response.function_call_arguments.delta"
        | "response.completed"
        | "response.failed"
        | "response.incomplete"
        | "error" => ResponsesStreamEventPolicy::MeaningfulConversion,
        "response.created"
        | "response.in_progress"
        | "keepalive"
        | "response.queued"
        | "response.content_part.added"
        | "response.content_part.done"
        | "response.output_text.done"
        | "response.refusal.done"
        | "response.function_call_arguments.done"
        | "response.reasoning_summary_part.added"
        | "response.reasoning_summary_part.done"
        | "response.file_search_call.in_progress"
        | "response.file_search_call.searching"
        | "response.file_search_call.completed"
        | "response.web_search_call.in_progress"
        | "response.web_search_call.searching"
        | "response.web_search_call.completed"
        | "response.image_generation_call.in_progress"
        | "response.image_generation_call.generating"
        | "response.image_generation_call.completed"
        | "response.mcp_call.in_progress"
        | "response.mcp_call.completed"
        | "response.mcp_list_tools.in_progress"
        | "response.mcp_list_tools.completed"
        | "response.code_interpreter_call.in_progress"
        | "response.code_interpreter_call.interpreting"
        | "response.code_interpreter_call.completed" => ResponsesStreamEventPolicy::DocumentedStatusMarkerNoop,
        "response.code_interpreter_call_code.delta"
        | "response.code_interpreter_call_code.done"
        | "response.mcp_call_arguments.delta"
        | "response.mcp_call_arguments.done"
        | "response.image_generation_call.partial_image"
        | "response.custom_tool_call_input.delta"
        | "response.custom_tool_call_input.done"
        | "response.output_text.annotation.added" => ResponsesStreamEventPolicy::DocumentedValueBearingRigGap,
        _ => ResponsesStreamEventPolicy::Unsupported,
    }
}

struct ResponsesNormalizedStreamProcessor<P> {
    options: ResponsesNormalizedStreamOptions,
    parse_final_response: P,
    aggregator: StreamAggregator,
    seen_tool_calls: HashSet<String>,
    tool_call_indexes: HashMap<String, usize>,
    tool_call_names: HashMap<String, String>,
    tool_call_ids_by_item_id: HashMap<String, String>,
    next_tool_call_index: usize,
    fabricated_ids: HashMap<usize, String>,
    fabricated_fallback_id: Option<String>,
    final_response: Option<Value>,
    reconciler: ResponsesStreamReconciler,
}

impl<P> ResponsesNormalizedStreamProcessor<P>
where
    P: Fn(Value) -> Result<LLMResponse, LLMError>,
{
    fn new(options: ResponsesNormalizedStreamOptions, parse_final_response: P) -> Self {
        Self {
            aggregator: StreamAggregator::new(options.model.clone()),
            options,
            parse_final_response,
            seen_tool_calls: HashSet::new(),
            tool_call_indexes: HashMap::new(),
            tool_call_names: HashMap::new(),
            tool_call_ids_by_item_id: HashMap::new(),
            next_tool_call_index: 0,
            fabricated_ids: HashMap::new(),
            fabricated_fallback_id: None,
            final_response: None,
            reconciler: ResponsesStreamReconciler::default(),
        }
    }

    /// Fabricate an id once per logical call and reuse it across deltas.
    /// Recomputing a fallback per delta fragments one id-less call into
    /// several builders, and index-based ids collide across responses.
    fn fabricated_call_id(&mut self, output_index: Option<usize>) -> String {
        match output_index {
            Some(index) => self.fabricated_ids.entry(index).or_insert_with(generate_tool_call_id).clone(),
            None => self.fabricated_fallback_id.get_or_insert_with(generate_tool_call_id).clone(),
        }
    }

    fn is_done(&self) -> bool {
        self.reconciler.terminal_state() != ResponsesTerminalState::Active
    }

    #[cfg(test)]
    fn handle_payload(&mut self, payload: Value) -> Result<Vec<NormalizedStreamEvent>, LLMError> {
        let serialized = serde_json::to_string(&payload)
            .map_err(|err| provider_error(self.options.provider_name, format!("invalid stream payload: {err}")))?;
        self.handle_payload_data(&serialized)
    }

    fn handle_payload_data(&mut self, payload: &str) -> Result<Vec<NormalizedStreamEvent>, LLMError> {
        let event = ResponsesStreamAdapter::parse_sse_data_for_provider(self.options.provider_name, payload)?;
        self.handle_event(event)
    }

    fn handle_event(&mut self, event: ResponsesStreamEvent) -> Result<Vec<NormalizedStreamEvent>, LLMError> {
        let mut events = Vec::new();
        if !self.reconciler.admit(event.sequence_number()) {
            return Ok(events);
        }

        match event {
            ResponsesStreamEvent::TextDelta { delta, .. } => {
                for event in self.aggregator.handle_content(&delta) {
                    match event {
                        crate::provider::LLMStreamEvent::Token { delta } => {
                            events.push(NormalizedStreamEvent::TextDelta { delta });
                        }
                        crate::provider::LLMStreamEvent::Reasoning { delta } if self.options.emit_reasoning => {
                            events.push(NormalizedStreamEvent::ReasoningDelta { delta });
                        }
                        _ => {}
                    }
                }
            }
            ResponsesStreamEvent::RefusalDelta { delta, .. } => {
                if !delta.is_empty() {
                    self.aggregator.content.push_str(&delta);
                    events.push(NormalizedStreamEvent::TextDelta { delta });
                }
            }
            ResponsesStreamEvent::ReasoningDelta { delta, item_id, output_index, sub_index, .. } => {
                let delta = self.reconciler.reasoning_delta(
                    ResponsesItemIdentity::new(item_id, None, output_index).with_sub_index(sub_index),
                    &delta,
                );
                if self.options.emit_reasoning && !delta.is_empty() {
                    self.aggregator.reasoning.push_str(&delta);
                    events.push(NormalizedStreamEvent::ReasoningDelta { delta });
                }
            }
            ResponsesStreamEvent::ReasoningDone { text, item_id, output_index, sub_index, .. } => {
                let delta = self
                    .reconciler
                    .reasoning_done(
                        ResponsesItemIdentity::new(item_id, None, output_index).with_sub_index(sub_index),
                        &text,
                    )
                    .map_err(|message| provider_error(self.options.provider_name, message))?;
                if self.options.emit_reasoning
                    && let Some(delta) = delta
                    && !delta.is_empty()
                {
                    self.aggregator.reasoning.push_str(&delta);
                    events.push(NormalizedStreamEvent::ReasoningDelta { delta });
                }
            }
            ResponsesStreamEvent::FunctionCallNameDelta { call_id, item_id, name, output_index, .. } => {
                self.record_tool_call_item_id(item_id.as_deref(), &call_id);
                self.record_tool_call_name(&call_id, &name, output_index);
                self.push_tool_call_start(&mut events, call_id, Some(name));
            }
            ResponsesStreamEvent::FunctionCallArgumentsDelta { call_id, item_id, delta, output_index, .. } => {
                let call_id = self.provider_tool_call_id(item_id.as_deref(), call_id);
                let call_id = if call_id.is_empty() {
                    self.fabricated_call_id(output_index)
                } else {
                    call_id
                };
                let index = self.resolve_tool_call_index(&call_id, output_index);

                let name = self.tool_call_names.get(&call_id).cloned();
                self.push_tool_call_start(&mut events, call_id.clone(), name);

                if !delta.is_empty() {
                    self.aggregator.handle_tool_calls(&[json!({
                        "index": index,
                        "id": call_id,
                        "function": {
                            "arguments": delta,
                        }
                    })]);
                    events.push(NormalizedStreamEvent::ToolCallDelta { call_id, delta });
                }
            }
            ResponsesStreamEvent::CompletedToolCall {
                call_id, item_id, name, arguments, output_index, ..
            } => {
                self.record_tool_call_item_id(item_id.as_deref(), &call_id);
                let index = self.record_tool_call_name(&call_id, &name, output_index);
                self.push_tool_call_start(&mut events, call_id.clone(), Some(name));
                self.aggregator.handle_tool_calls(&[json!({
                    "index": index,
                    "id": call_id,
                    "function": {
                        "arguments": arguments,
                    }
                })]);
            }
            ResponsesStreamEvent::CustomToolCall { item_id, call_id, name, input, output_index, .. } => {
                let identity = ResponsesItemIdentity::new(item_id, call_id, output_index);
                let progress_call_id = identity.response_call_id().map(ToOwned::to_owned);
                self.reconciler
                    .capture_custom_call(identity, name.as_deref(), input.as_deref())
                    .map_err(|message| provider_error(self.options.provider_name, message))?;
                if let Some(call_id) = progress_call_id {
                    self.push_tool_call_start(&mut events, call_id, name);
                }
            }
            ResponsesStreamEvent::CustomToolCallInputDelta { item_id, call_id, delta, output_index, .. } => {
                let identity = ResponsesItemIdentity::new(item_id, call_id, output_index);
                let progress_call_id = identity
                    .response_call_id()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| provider_error(self.options.provider_name, "custom tool input has no call id"))?;
                let delta = self
                    .reconciler
                    .custom_input_delta(identity, &delta)
                    .map_err(|message| provider_error(self.options.provider_name, message))?;
                self.push_tool_call_start(&mut events, progress_call_id.clone(), None);
                if !delta.is_empty() {
                    events.push(NormalizedStreamEvent::ToolCallDelta { call_id: progress_call_id, delta });
                }
            }
            ResponsesStreamEvent::CustomToolCallInputDone { item_id, call_id, input, output_index, .. } => {
                let identity = ResponsesItemIdentity::new(item_id, call_id, output_index);
                let progress_call_id = identity
                    .response_call_id()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| provider_error(self.options.provider_name, "custom tool input has no call id"))?;
                let delta = self
                    .reconciler
                    .custom_input_done(identity, &input)
                    .map_err(|message| provider_error(self.options.provider_name, message))?;
                self.push_tool_call_start(&mut events, progress_call_id.clone(), None);
                if let Some(delta) = delta
                    && !delta.is_empty()
                {
                    events.push(NormalizedStreamEvent::ToolCallDelta { call_id: progress_call_id, delta });
                }
            }
            ResponsesStreamEvent::CompletedResponse { response, .. } => {
                responses_validation::validate_completed_response(&response)?;
                self.final_response = Some(response);
                self.reconciler.mark_completed();
            }
            ResponsesStreamEvent::Error { message, .. } => {
                self.reconciler.mark_failed();
                return Err(provider_error(self.options.provider_name, message));
            }
            ResponsesStreamEvent::Lifecycle { .. }
            | ResponsesStreamEvent::ProviderValueBearingRigGap { .. }
            | ResponsesStreamEvent::Unknown { .. } => {}
        }

        Ok(events)
    }

    fn record_tool_call_name(&mut self, call_id: &str, name: &str, output_index: Option<usize>) -> usize {
        self.tool_call_names
            .entry(call_id.to_string())
            .or_insert_with(|| name.to_string());
        let index = self.resolve_tool_call_index(call_id, output_index);
        self.aggregator.handle_tool_calls(&[json!({
            "index": index,
            "id": call_id,
            "function": {
                "name": name,
            }
        })]);
        index
    }

    fn finish(self) -> Result<Vec<NormalizedStreamEvent>, LLMError> {
        self.reconciler
            .require_completed()
            .map_err(|message| provider_error(self.options.provider_name, message))?;
        let final_response = self.final_response.ok_or_else(|| {
            provider_error(self.options.provider_name, "response.completed event did not contain a response")
        })?;

        let mut streamed = self.aggregator.finalize();
        merge_reconciled_custom_calls(&mut streamed, &self.reconciler, self.options.provider_name)?;
        let mut response = match (self.parse_final_response)(final_response.clone()) {
            Ok(response) => response,
            Err(_) if final_response_output_is_empty(&final_response) && streamed_response_is_usable(&streamed) => {
                streamed.clone()
            }
            Err(err) => return Err(err),
        };
        merge_final_response_metadata(&mut response, &final_response, self.options.include_cached_prompt_metrics)?;
        if final_response_output_is_empty(&final_response) && response.tool_calls.is_none() {
            response.tool_calls = streamed.tool_calls.clone();
        }

        merge_streamed_response(
            &mut response,
            streamed,
            self.options.provider_name,
            self.options.allow_function_call_id_remap,
        )?;

        let mut events = Vec::new();
        if let Some(usage) = response.usage.clone() {
            events.push(NormalizedStreamEvent::Usage { usage });
        }
        events.push(NormalizedStreamEvent::Done { response: Box::new(response) });
        Ok(events)
    }

    fn record_tool_call_item_id(&mut self, item_id: Option<&str>, call_id: &str) {
        let Some(item_id) = item_id.filter(|item_id| !item_id.is_empty()) else {
            return;
        };

        self.tool_call_ids_by_item_id
            .entry(item_id.to_string())
            .or_insert_with(|| call_id.to_string());
    }

    fn provider_tool_call_id(&self, item_id: Option<&str>, call_id: String) -> String {
        item_id
            .and_then(|item_id| self.tool_call_ids_by_item_id.get(item_id))
            .or_else(|| self.tool_call_ids_by_item_id.get(call_id.as_str()))
            .cloned()
            .unwrap_or(call_id)
    }

    fn push_tool_call_start(&mut self, events: &mut Vec<NormalizedStreamEvent>, call_id: String, name: Option<String>) {
        if self.seen_tool_calls.insert(call_id.clone()) {
            events.push(NormalizedStreamEvent::ToolCallStart { call_id, name });
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

pub fn create_responses_normalized_stream<P>(
    response: reqwest::Response,
    options: ResponsesNormalizedStreamOptions,
    parse_final_response: P,
) -> LLMNormalizedStream
where
    P: Fn(Value) -> Result<LLMResponse, LLMError> + Send + 'static,
{
    let stream = try_stream! {
        let provider_name = options.provider_name;
        let mut processor = ResponsesNormalizedStreamProcessor::new(options, parse_final_response);
        let mut body_stream = response.bytes_stream();
        let mut decoder = ResponsesSseDecoder::default();

        while let Some(chunk_result) = body_stream.next().await {
            let chunk = chunk_result.map_err(|err| provider_error(
                provider_name,
                format!("streaming error: {err}"),
            ))?;
            let data_payloads = decoder.push(&chunk);

            for data_payload in data_payloads {
                let data_payload = data_payload.map_err(|error| error.into_llm_error(provider_name))?;
                let trimmed_payload = data_payload.trim();
                if trimmed_payload.is_empty() || trimmed_payload == "[DONE]" {
                    continue;
                }

                for event in processor.handle_payload_data(trimmed_payload)? {
                    yield event;
                }

                if processor.is_done() {
                    break;
                }
            }

            if processor.is_done() {
                break;
            }
        }

        if !processor.is_done() {
            decoder.finish().map_err(|error| error.into_llm_error(provider_name))?;
        }

        for event in processor.finish()? {
            yield event;
        }
    };

    Box::pin(stream)
}

fn streamed_response_is_usable(response: &LLMResponse) -> bool {
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

fn merge_streamed_response(
    response: &mut LLMResponse,
    streamed: LLMResponse,
    provider_name: &str,
    allow_function_call_id_remap: bool,
) -> Result<(), LLMError> {
    if response.content.as_deref().unwrap_or_default().is_empty() {
        response.content = streamed.content;
    } else if let (Some(content), Some(streamed_content)) = (&mut response.content, streamed.content)
        && !streamed_content.is_empty()
        && !content.contains(&streamed_content)
    {
        content.push_str(&streamed_content);
    }

    reconcile_streamed_tool_calls(
        response,
        streamed.tool_calls.as_deref(),
        provider_name,
        allow_function_call_id_remap,
    )?;

    if response.usage.is_none() {
        response.usage = streamed.usage;
    }

    if response.reasoning.is_none() {
        response.reasoning = streamed.reasoning;
    }

    if response.reasoning_details.is_none() {
        response.reasoning_details = streamed.reasoning_details;
    }

    if response.tool_references.is_empty() && !streamed.tool_references.is_empty() {
        response.tool_references = streamed.tool_references;
    }

    if response.request_id.is_none() {
        response.request_id = streamed.request_id;
    }

    if response.organization_id.is_none() {
        response.organization_id = streamed.organization_id;
    }
    Ok(())
}

fn merge_reconciled_custom_calls(
    response: &mut LLMResponse,
    reconciler: &ResponsesStreamReconciler,
    provider_name: &str,
) -> Result<(), LLMError> {
    let streamed_calls = reconciler.custom_tool_calls();
    if streamed_calls.is_empty() {
        return Ok(());
    }

    let response_calls = response.tool_calls.get_or_insert_default();
    for streamed_call in streamed_calls {
        let tool_call = ToolCall::custom(streamed_call.call_id, streamed_call.name, streamed_call.input);
        if let Some(existing) = response_calls.iter_mut().find(|call| call.id == tool_call.id) {
            reconcile_tool_call(existing, &tool_call, provider_name)?;
        } else {
            response_calls.push(tool_call);
        }
    }
    Ok(())
}

fn reconcile_streamed_tool_calls(
    response: &mut LLMResponse,
    streamed_calls: Option<&[ToolCall]>,
    provider_name: &str,
    allow_function_call_id_remap: bool,
) -> Result<(), LLMError> {
    let Some(streamed_calls) = streamed_calls.filter(|calls| !calls.is_empty()) else {
        return Ok(());
    };
    let Some(response_calls) = response.tool_calls.as_mut() else {
        return Err(provider_error(
            provider_name,
            "completed response tool calls do not map one-to-one to streamed calls",
        ));
    };

    let correlations = correlate_streamed_function_calls(response_calls, streamed_calls, allow_function_call_id_remap)
        .map_err(|message| provider_error(provider_name, message))?;
    for correlation in correlations {
        let final_call = &mut response_calls[correlation.final_index];
        let streamed_call = &streamed_calls[correlation.streamed_index];
        if final_call.id == streamed_call.id {
            reconcile_tool_call(final_call, streamed_call, provider_name)?;
        }
    }
    Ok(())
}

fn reconcile_tool_call(
    final_call: &mut ToolCall,
    streamed_call: &ToolCall,
    provider_name: &str,
) -> Result<(), LLMError> {
    if final_call.call_type != streamed_call.call_type || final_call.tool_name() != streamed_call.tool_name() {
        return Err(provider_error(provider_name, "completed tool metadata contradicts streamed metadata"));
    }
    let final_input = final_call.raw_input().unwrap_or_default();
    let streamed_input = streamed_call.raw_input().unwrap_or_default();
    match reconcile_final_input(streamed_input, final_input)
        .map_err(|message| provider_error(provider_name, message))?
    {
        FinalInputPreference::Final => {}
        FinalInputPreference::Streamed => {
            let final_id = std::mem::take(&mut final_call.id);
            *final_call = streamed_call.clone();
            final_call.id = final_id;
        }
    }
    Ok(())
}

fn merge_final_response_metadata(
    response: &mut LLMResponse,
    final_response: &Value,
    include_cached_prompt_metrics: bool,
) -> Result<(), LLMError> {
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

fn provider_error(provider_name: &str, message: impl Into<String>) -> LLMError {
    let message = error_display::format_llm_error(provider_name, &message.into());
    LLMError::Provider { message, metadata: None }
}

#[cfg(test)]
mod tests {
    use super::{
        ResponsesNormalizedStreamOptions, ResponsesNormalizedStreamProcessor, ResponsesStreamEventPolicy,
        merge_streamed_response, provider_error, reconcile_streamed_tool_calls, response_stream_event_policy,
    };
    use crate::provider::{FinishReason, LLMResponse, NormalizedStreamEvent, ToolCall};
    use serde_json::{Value, json};

    #[test]
    fn reasoning_part_events_are_meaningful_conversions() {
        for event_type in ["response.reasoning_part.added", "response.reasoning_part.done"] {
            assert_eq!(
                response_stream_event_policy(&json!({"type": event_type})),
                Ok(ResponsesStreamEventPolicy::MeaningfulConversion)
            );
        }
    }

    fn options() -> ResponsesNormalizedStreamOptions {
        ResponsesNormalizedStreamOptions {
            provider_name: "TestProvider",
            model: "gpt-5".to_string(),
            emit_reasoning: true,
            include_cached_prompt_metrics: false,
            allow_function_call_id_remap: false,
        }
    }

    fn parse_response(value: Value) -> Result<LLMResponse, crate::provider::LLMError> {
        let content = value
            .get("output")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("content"))
            .and_then(Value::as_array)
            .and_then(|content| content.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        Ok(LLMResponse {
            content,
            model: "gpt-5".to_string(),
            finish_reason: FinishReason::Stop,
            ..Default::default()
        })
    }

    fn response_fixture(status: &str, output: Value, usage: Value) -> Value {
        json!({
            "id": "resp_test",
            "object": "response",
            "created_at": 1,
            "status": status,
            "error": null,
            "incomplete_details": null,
            "instructions": null,
            "max_output_tokens": null,
            "model": "gpt-5",
            "usage": usage,
            "output": output,
            "tools": []
        })
    }

    fn completed_response_fixture(output: Value) -> Value {
        response_fixture("completed", output, Value::Null)
    }

    fn completion_event(sequence_number: u64, output: Value) -> Value {
        json!({
            "type": "response.completed",
            "sequence_number": sequence_number,
            "response": completed_response_fixture(output)
        })
    }

    fn text_delta_fixture(delta: &str) -> Value {
        json!({
            "type": "response.output_text.delta",
            "item_id": "msg_1",
            "output_index": 0,
            "content_index": 0,
            "sequence_number": 1,
            "delta": delta
        })
    }

    fn refusal_delta_fixture(delta: &str) -> Value {
        json!({
            "type": "response.refusal.delta",
            "item_id": "msg_1",
            "output_index": 0,
            "content_index": 0,
            "sequence_number": 1,
            "delta": delta
        })
    }

    #[test]
    fn text_delta_and_completed_yield_text_then_done() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), parse_response);

        let events = processor
            .handle_payload(text_delta_fixture("hello"))
            .expect("text delta should parse");
        assert!(matches!(
            events.as_slice(),
            [NormalizedStreamEvent::TextDelta { delta }] if delta == "hello"
        ));

        let completed_events = processor
            .handle_payload(json!({
                "type": "response.completed",
                "sequence_number": 2,
                "response": completed_response_fixture(json!([{
                        "type": "message",
                        "id": "msg_1",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "hello"}]
                    }]))
            }))
            .expect("completed event should parse");
        assert!(completed_events.is_empty());

        let finished = processor.finish().expect("finish should succeed");
        assert!(matches!(
            finished.as_slice(),
            [NormalizedStreamEvent::Done { response }]
                if response.content.as_deref() == Some("hello")
        ));
    }

    #[test]
    fn empty_final_response_uses_streamed_text_and_preserves_metadata() {
        let mut options = options();
        options.include_cached_prompt_metrics = true;
        let mut processor = ResponsesNormalizedStreamProcessor::new(options, |value| {
            let output = value
                .get("output")
                .and_then(Value::as_array)
                .ok_or_else(|| provider_error("TestProvider", "missing output"))?;
            if output.is_empty() {
                return Err(provider_error("TestProvider", "No output in response"));
            }
            parse_response(value)
        });

        processor
            .handle_payload(text_delta_fixture("streamed answer"))
            .expect("text delta should parse");
        processor
            .handle_payload(json!({
                "type": "response.completed",
                "sequence_number": 2,
                "response": {
                    "id": "resp_streamed",
                    "object": "response",
                    "created_at": 1,
                    "status": "completed",
                    "error": null,
                    "incomplete_details": null,
                    "instructions": null,
                    "max_output_tokens": null,
                    "model": "gpt-5",
                    "usage": {
                        "input_tokens": 11,
                        "output_tokens": 7,
                        "total_tokens": 18,
                        "input_tokens_details": {
                            "cached_tokens": 5
                        }
                    },
                    "output": [],
                    "tools": []
                }
            }))
            .expect("completed event should parse");

        let finished = processor.finish().expect("finish should succeed");
        let [
            NormalizedStreamEvent::Usage { usage },
            NormalizedStreamEvent::Done { response },
        ] = finished.as_slice()
        else {
            panic!("expected usage then done");
        };
        assert_eq!(usage.prompt_tokens, 11);
        assert_eq!(usage.completion_tokens, 7);
        assert_eq!(usage.total_tokens, 18);
        assert_eq!(usage.cached_prompt_tokens, Some(5));
        assert_eq!(response.content.as_deref(), Some("streamed answer"));
        assert_eq!(response.request_id.as_deref(), Some("resp_streamed"));
    }

    #[test]
    fn tool_call_deltas_emit_start_and_finish_with_assembled_tool_call() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), |_| {
            Ok(LLMResponse { model: "gpt-5".to_string(), ..Default::default() })
        });

        let started = processor
            .handle_payload(json!({
                "type": "response.output_item.added",
                "item_id": "call_1",
                "output_index": 0,
                "sequence_number": 1,
                "item": {
                    "type": "function_call",
                    "id": "call_1",
                    "call_id": "call_1",
                    "name": "search_workspace",
                    "arguments": "",
                    "status": "in_progress"
                }
            }))
            .expect("output item metadata should parse");
        assert!(matches!(
            started.as_slice(),
            [NormalizedStreamEvent::ToolCallStart { call_id, name }]
                if call_id == "call_1" && name.as_deref() == Some("search_workspace")
        ));

        let first = processor
            .handle_payload(json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "call_1",
                "output_index": 0,
                "content_index": 0,
                "sequence_number": 2,
                "delta": "{\"query\":\"vt"
            }))
            .expect("first tool delta should parse");
        assert!(matches!(
            first.as_slice(),
            [NormalizedStreamEvent::ToolCallDelta { call_id: delta_call_id, delta }]
            if delta_call_id == "call_1"
                && delta == "{\"query\":\"vt"
        ));

        let second = processor
            .handle_payload(json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "call_1",
                "output_index": 0,
                "content_index": 0,
                "sequence_number": 3,
                "delta": "code\"}"
            }))
            .expect("second tool delta should parse");
        assert!(matches!(
            second.as_slice(),
            [NormalizedStreamEvent::ToolCallDelta { call_id, delta }]
                if call_id == "call_1" && delta == "code\"}"
        ));

        processor
            .handle_payload(completion_event(4, json!([])))
            .expect("completion should parse");

        let finished = processor.finish().expect("finish should succeed");
        let response = match finished.as_slice() {
            [NormalizedStreamEvent::Done { response }] => response,
            _ => panic!("expected done event"),
        };
        let tool_calls = response.tool_calls.as_ref().expect("tool call should be assembled");
        assert_eq!(
            tool_calls,
            &vec![ToolCall::function(
                "call_1".to_string(),
                "search_workspace".to_string(),
                "{\"query\":\"vtcode\"}".to_string(),
            )]
        );
    }

    #[test]
    fn tool_call_delta_without_ids_fabricates_and_reuses_id() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), |_| {
            Ok(LLMResponse { model: "gpt-5".to_string(), ..Default::default() })
        });

        let delta_payload = |sequence_number: u64, fragment: &str| {
            json!({
                "type": "response.function_call_arguments.delta",
                "output_index": 0,
                "content_index": 0,
                "sequence_number": sequence_number,
                "delta": fragment
            })
        };

        let first = processor
            .handle_payload(delta_payload(1, "{\"query\":"))
            .expect("first tool delta should parse");
        let first_id = match first.as_slice() {
            [
                NormalizedStreamEvent::ToolCallStart { call_id, .. },
                NormalizedStreamEvent::ToolCallDelta { call_id: delta_id, .. },
            ] => {
                assert_eq!(call_id, delta_id);
                call_id.clone()
            }
            other => panic!("unexpected events: {other:?}"),
        };
        assert!(first_id.starts_with("call_"));

        let second = processor
            .handle_payload(delta_payload(2, "\"vtcode\"}"))
            .expect("second tool delta should parse");
        assert!(matches!(
            second.as_slice(),
            [NormalizedStreamEvent::ToolCallDelta { call_id, .. }] if *call_id == first_id
        ));
    }

    #[test]
    fn tool_call_delta_without_ids_fabricates_distinct_ids_per_processor() {
        let make_id = || {
            let mut processor = ResponsesNormalizedStreamProcessor::new(options(), |_| {
                Ok(LLMResponse { model: "gpt-5".to_string(), ..Default::default() })
            });
            let events = processor
                .handle_payload(json!({
                    "type": "response.function_call_arguments.delta",
                    "output_index": 0,
                    "content_index": 0,
                    "sequence_number": 1,
                    "delta": "{}"
                }))
                .expect("tool delta should parse");
            match events.as_slice() {
                [NormalizedStreamEvent::ToolCallStart { call_id, .. }, ..] => call_id.clone(),
                other => panic!("unexpected events: {other:?}"),
            }
        };

        assert_ne!(make_id(), make_id(), "fabricated ids must differ across responses");
    }

    #[test]
    fn tool_call_deltas_use_provider_call_id_when_item_id_differs() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), |_| {
            Ok(LLMResponse { model: "gpt-5".to_string(), ..Default::default() })
        });

        let started = processor
            .handle_payload(json!({
                "type": "response.output_item.added",
                "item_id": "fc_1",
                "output_index": 0,
                "sequence_number": 1,
                "item": {
                    "type": "function_call",
                    "id": "fc_1",
                    "call_id": "call_1",
                    "name": "search_workspace",
                    "arguments": "",
                    "status": "in_progress"
                }
            }))
            .expect("output item metadata should parse");
        assert!(matches!(
            started.as_slice(),
            [NormalizedStreamEvent::ToolCallStart { call_id, name }]
                if call_id == "call_1" && name.as_deref() == Some("search_workspace")
        ));

        let first = processor
            .handle_payload(json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_1",
                "output_index": 0,
                "content_index": 0,
                "sequence_number": 2,
                "delta": "{\"query\":\"vt"
            }))
            .expect("first tool delta should parse");
        assert!(matches!(
            first.as_slice(),
            [NormalizedStreamEvent::ToolCallDelta { call_id, delta }]
                if call_id == "call_1" && delta == "{\"query\":\"vt"
        ));

        processor
            .handle_payload(json!({
                "type": "response.function_call_arguments.delta",
                "item_id": "fc_1",
                "output_index": 0,
                "content_index": 0,
                "sequence_number": 3,
                "delta": "code\"}"
            }))
            .expect("second tool delta should parse");

        processor
            .handle_payload(completion_event(4, json!([])))
            .expect("completion should parse");

        let finished = processor.finish().expect("finish should succeed");
        let response = match finished.as_slice() {
            [NormalizedStreamEvent::Done { response }] => response,
            _ => panic!("expected done event"),
        };
        let tool_calls = response.tool_calls.as_ref().expect("tool call should be assembled");
        assert_eq!(
            tool_calls,
            &vec![ToolCall::function(
                "call_1".to_string(),
                "search_workspace".to_string(),
                "{\"query\":\"vtcode\"}".to_string(),
            )]
        );
    }

    #[test]
    fn custom_tool_input_stream_events_wait_for_completed_response_replay() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), |value| {
            let custom_call = value
                .get("output")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .expect("custom tool output should exist");

            Ok(LLMResponse {
                model: "gpt-5".to_string(),
                finish_reason: FinishReason::ToolCalls,
                tool_calls: Some(vec![ToolCall::custom(
                    custom_call
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    custom_call.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                    custom_call.get("input").and_then(Value::as_str).unwrap_or_default().to_string(),
                )]),
                ..Default::default()
            })
        });

        let delta_events = processor
            .handle_payload(json!({
                "type": "response.custom_tool_call_input.delta",
                "sequence_number": 1,
                "item_id": "ct_1",
                "call_id": "call_patch_1",
                "output_index": 0,
                "delta": "*** Begin"
            }))
            .expect("custom tool input delta should parse");
        assert!(matches!(
            delta_events.as_slice(),
            [
                NormalizedStreamEvent::ToolCallStart { call_id, name: None },
                NormalizedStreamEvent::ToolCallDelta { call_id: delta_call_id, delta },
            ] if call_id == "call_patch_1" && delta_call_id == call_id && delta == "*** Begin"
        ));

        let done_events = processor
            .handle_payload(json!({
                "type": "response.custom_tool_call_input.done",
                "sequence_number": 2,
                "item_id": "ct_1",
                "call_id": "call_patch_1",
                "output_index": 0,
                "input": "*** Begin Patch\n*** End Patch\n"
            }))
            .expect("custom tool input done should parse");
        assert!(matches!(
            done_events.as_slice(),
            [NormalizedStreamEvent::ToolCallDelta { call_id, delta }]
                if call_id == "call_patch_1" && delta == " Patch\n*** End Patch\n"
        ));

        let completed_events = processor
            .handle_payload(json!({
                "type": "response.completed",
                "sequence_number": 3,
                "response": completed_response_fixture(json!([{
                    "type": "custom_tool_call",
                    "id": "ct_1",
                    "call_id": "call_patch_1",
                    "name": "apply_patch",
                    "input": "*** Begin Patch\n*** End Patch\n"
                }]))
            }))
            .expect("completed event should parse");
        assert!(completed_events.is_empty());

        let finished = processor.finish().expect("finish should succeed");
        let response = match finished.as_slice() {
            [NormalizedStreamEvent::Done { response }] => response,
            _ => panic!("expected final done event"),
        };
        let tool_calls = response
            .tool_calls
            .as_ref()
            .expect("custom tool call should be replayed from final response");
        assert_eq!(tool_calls.len(), 1);
        assert!(tool_calls[0].is_custom());
        assert_eq!(tool_calls[0].id, "call_patch_1");
        assert_eq!(tool_calls[0].tool_name(), Some("apply_patch"));
        assert_eq!(tool_calls[0].raw_input(), Some("*** Begin Patch\n*** End Patch\n"));
    }

    #[test]
    fn refusal_delta_streams_visible_output() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), parse_response);

        let events = processor
            .handle_payload(refusal_delta_fixture("I can't help with that"))
            .expect("refusal delta should parse");
        assert!(matches!(
            events.as_slice(),
            [NormalizedStreamEvent::TextDelta { delta }]
                if delta == "I can't help with that"
        ));

        processor
            .handle_payload(completion_event(2, json!([])))
            .expect("completion should parse");

        let finished = processor.finish().expect("finish should succeed");
        assert!(matches!(
            finished.as_slice(),
            [NormalizedStreamEvent::Done { response }]
                if response.content.as_deref() == Some("I can't help with that")
        ));
    }

    #[test]
    fn failed_incomplete_and_error_events_surface_backend_message() {
        for payload in [
            json!({
                "type": "response.failed",
                "sequence_number": 1,
                "response": {
                    "id": "resp_failed",
                    "object": "response",
                    "created_at": 1,
                    "status": "failed",
                    "error": {"code": "failed", "message": "failed"},
                    "incomplete_details": null,
                    "instructions": null,
                    "max_output_tokens": null,
                    "model": "gpt-5",
                    "usage": null,
                    "output": [],
                    "tools": []
                }
            }),
            json!({
                "type": "response.incomplete",
                "sequence_number": 1,
                "response": {
                    "id": "resp_incomplete",
                    "object": "response",
                    "created_at": 1,
                    "status": "incomplete",
                    "error": {"code": "incomplete", "message": "incomplete"},
                    "incomplete_details": {"reason": "incomplete"},
                    "instructions": null,
                    "max_output_tokens": null,
                    "model": "gpt-5",
                    "usage": null,
                    "output": [],
                    "tools": []
                }
            }),
            json!({"type": "error", "error": {"message": "errored"}}),
        ] {
            let mut processor = ResponsesNormalizedStreamProcessor::new(options(), parse_response);
            let error = processor.handle_payload(payload).expect_err("error payload should fail");
            assert!(
                error.to_string().contains("failed")
                    || error.to_string().contains("incomplete")
                    || error.to_string().contains("errored")
            );
        }
    }

    #[test]
    fn completed_event_rejects_contradictory_response_status() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), parse_response);
        let error = processor
            .handle_payload(json!({
                "type": "response.completed",
                "sequence_number": 1,
                "response": response_fixture("incomplete", json!([]), Value::Null)
            }))
            .expect_err("explicit incomplete status cannot become a successful terminal");

        assert!(error.to_string().contains("incomplete"));
        assert!(processor.finish().is_err(), "rejected status must not mark completion");
    }

    #[test]
    fn documented_non_runtime_events_are_ignored() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), parse_response);
        let events = processor
            .handle_payload(json!({
                "type": "response.file_search_call.searching",
                "query": "needle"
            }))
            .expect("documented status event should be ignored");
        assert!(events.is_empty());
        processor
            .handle_payload(json!({
                "type": "response.code_interpreter_call_code.delta",
                "item_id": "ci_1",
                "output_index": 0,
                "sequence_number": 2,
                "delta": "print(1)"
            }))
            .expect("documented code interpreter value-bearing event should be ignored downstream");

        processor
            .handle_payload(completion_event(3, json!([])))
            .expect("completion should parse");

        let finished = processor.finish().expect("finish should succeed");
        assert!(matches!(finished.as_slice(), [NormalizedStreamEvent::Done { .. }]));
    }

    #[test]
    fn missing_delta_reports_provider_error() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), parse_response);
        let error = processor
            .handle_payload(json!({
                "type": "response.output_text.delta",
                "item_id": "msg_1",
                "output_index": 0,
                "content_index": 0,
                "sequence_number": 1
            }))
            .expect_err("missing delta should fail");
        assert!(error.to_string().contains("TestProvider"));
        assert!(error.to_string().contains("invalid stream payload"));
    }

    #[test]
    fn eof_without_completed_terminal_is_an_error_even_after_valid_deltas() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), parse_response);
        processor
            .handle_payload(text_delta_fixture("partial"))
            .expect("text delta should parse");

        let error = processor.finish().expect_err("EOF without response.completed must fail");
        assert!(error.to_string().contains("response.completed"));
    }

    #[test]
    fn terminal_completion_absorbs_later_frames() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), parse_response);
        processor
            .handle_payload(completion_event(
                1,
                json!([{
                    "type": "message",
                    "content": [{"type": "output_text", "text": "final"}]
                }]),
            ))
            .expect("completion should parse");

        let late = processor
            .handle_payload(json!({
                "type": "response.output_text.delta",
                "sequence_number": 2,
                "item_id": "msg_1",
                "output_index": 0,
                "content_index": 0,
                "delta": "late"
            }))
            .expect("late frame should be absorbed");
        assert!(late.is_empty());

        let finished = processor.finish().expect("completed stream should finish");
        assert!(matches!(
            finished.as_slice(),
            [NormalizedStreamEvent::Done { response }] if response.content.as_deref() == Some("final")
        ));
    }

    #[test]
    fn reasoning_done_reconciles_snapshot_without_dropping_equal_real_deltas() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), parse_response);
        for sequence_number in [1, 2] {
            let events = processor
                .handle_payload(json!({
                    "type": "response.reasoning_text.delta",
                    "sequence_number": sequence_number,
                    "item_id": "reasoning_1",
                    "output_index": 0,
                    "delta": "ha"
                }))
                .expect("reasoning delta should parse");
            assert!(matches!(
                events.as_slice(),
                [NormalizedStreamEvent::ReasoningDelta { delta }] if delta == "ha"
            ));
        }

        let done = processor
            .handle_payload(json!({
                "type": "response.reasoning_text.done",
                "sequence_number": 3,
                "item_id": "reasoning_1",
                "output_index": 0,
                "text": "haha"
            }))
            .expect("reasoning done should parse");
        assert!(done.is_empty(), "done snapshot must not replay streamed reasoning");
        processor
            .handle_payload(completion_event(4, json!([])))
            .expect("completion should parse");

        let finished = processor.finish().expect("completed stream should finish");
        assert!(matches!(
            finished.as_slice(),
            [NormalizedStreamEvent::Done { response }] if response.reasoning.as_deref() == Some("haha")
        ));
    }

    #[test]
    fn reasoning_part_snapshots_preserve_prefix_without_replaying_done_text() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), parse_response);
        let frames = [
            json!({"type":"response.reasoning_part.added","sequence_number":0,"item_id":"r","output_index":0,"content_index":0,"part":{"type":"reasoning_text","text":"hel"}}),
            json!({"type":"response.reasoning_text.delta","sequence_number":1,"item_id":"r","output_index":0,"content_index":0,"delta":"lo"}),
            json!({"type":"response.reasoning_text.done","sequence_number":2,"item_id":"r","output_index":0,"content_index":0,"text":"hello"}),
            json!({"type":"response.reasoning_part.done","sequence_number":3,"item_id":"r","output_index":0,"content_index":0,"part":{"type":"reasoning_text","text":"hello"}}),
        ];
        let mut visible = String::new();
        for frame in frames {
            for event in processor.handle_payload(frame).expect("reasoning part frame") {
                if let NormalizedStreamEvent::ReasoningDelta { delta } = event {
                    visible.push_str(&delta);
                }
            }
        }
        assert_eq!(visible, "hello");
        processor.handle_payload(completion_event(4, json!([]))).expect("completion");
        let finished = processor.finish().expect("completed stream");
        assert!(matches!(finished.as_slice(), [NormalizedStreamEvent::Done { response }]
            if response.reasoning.as_deref() == Some("hello")));
    }

    #[test]
    fn interleaved_reasoning_summary_parts_reconcile_by_summary_index() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), parse_response);
        for (sequence_number, summary_index, delta) in [(1, 0, "first"), (2, 1, "second")] {
            processor
                .handle_payload(json!({
                    "type": "response.reasoning_summary_text.delta",
                    "sequence_number": sequence_number,
                    "item_id": "reasoning_1",
                    "output_index": 0,
                    "summary_index": summary_index,
                    "delta": delta
                }))
                .expect("summary delta should parse");
        }

        let first_done = processor
            .handle_payload(json!({
                "type": "response.reasoning_summary_text.done",
                "sequence_number": 3,
                "item_id": "reasoning_1",
                "output_index": 0,
                "summary_index": 0,
                "delta": "first+"
            }))
            .expect("first summary done should parse");
        let second_done = processor
            .handle_payload(json!({
                "type": "response.reasoning_summary_text.done",
                "sequence_number": 4,
                "item_id": "reasoning_1",
                "output_index": 0,
                "summary_index": 1,
                "delta": "second+"
            }))
            .expect("second summary done should parse");

        assert!(matches!(
            first_done.as_slice(),
            [NormalizedStreamEvent::ReasoningDelta { delta }] if delta == "+"
        ));
        assert!(matches!(
            second_done.as_slice(),
            [NormalizedStreamEvent::ReasoningDelta { delta }] if delta == "+"
        ));
    }

    #[test]
    fn streamed_custom_input_recovers_empty_completed_output_without_early_dispatch() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), |value| {
            if value.get("output").and_then(Value::as_array).is_some_and(Vec::is_empty) {
                return Err(provider_error("TestProvider", "No output in response"));
            }
            parse_response(value)
        });
        for (position, payload) in [
            json!({
                "type": "response.output_item.added",
                "sequence_number": 1,
                "item_id": "custom_1",
                "output_index": 0,
                "item": {
                    "type": "custom_tool_call",
                    "id": "custom_1",
                    "call_id": "call_custom_1",
                    "name": "apply_patch",
                    "input": "",
                    "status": "in_progress"
                }
            }),
            json!({
                "type": "response.custom_tool_call_input.delta",
                "sequence_number": 2,
                "item_id": "custom_1",
                "call_id": "call_custom_1",
                "output_index": 0,
                "delta": "*** Begin"
            }),
            json!({
                "type": "response.custom_tool_call_input.done",
                "sequence_number": 3,
                "item_id": "custom_1",
                "call_id": "call_custom_1",
                "output_index": 0,
                "input": "*** Begin Patch\n*** End Patch\n"
            }),
        ]
        .into_iter()
        .enumerate()
        {
            let events = processor.handle_payload(payload).expect("custom stream event should parse");
            assert!(
                events.iter().all(|event| matches!(
                    event,
                    NormalizedStreamEvent::ToolCallStart { .. } | NormalizedStreamEvent::ToolCallDelta { .. }
                )),
                "custom input may report progress but is not a terminal dispatch"
            );
            assert!(!events.is_empty(), "fixture {position} should report progress");
        }
        processor
            .handle_payload(completion_event(4, json!([])))
            .expect("completion should parse");

        let finished = processor.finish().expect("completed stream should finish");
        let response = match finished.as_slice() {
            [NormalizedStreamEvent::Done { response }] => response,
            other => panic!("unexpected events: {other:?}"),
        };
        let calls = response.tool_calls.as_ref().expect("reconciled custom call");
        assert_eq!(calls.len(), 1);
        assert!(calls[0].is_custom());
        assert_eq!(calls[0].id, "call_custom_1");
        assert_eq!(calls[0].tool_name(), Some("apply_patch"));
        assert_eq!(calls[0].raw_input(), Some("*** Begin Patch\n*** End Patch\n"));
    }

    #[test]
    fn contradictory_custom_call_aliases_fail_before_completion() {
        let mut processor = ResponsesNormalizedStreamProcessor::new(options(), parse_response);
        processor
            .handle_payload(json!({
                "type": "response.output_item.added",
                "sequence_number": 1,
                "item_id": "custom_1",
                "output_index": 0,
                "item": {
                    "type": "custom_tool_call",
                    "id": "custom_1",
                    "call_id": "call_original",
                    "name": "apply_patch",
                    "input": "",
                    "status": "in_progress"
                }
            }))
            .expect("custom metadata should parse");

        let error = processor
            .handle_payload(json!({
                "type": "response.custom_tool_call_input.delta",
                "sequence_number": 2,
                "item_id": "custom_1",
                "call_id": "call_changed",
                "output_index": 0,
                "delta": "unsafe"
            }))
            .expect_err("conflicting aliases must fail");
        assert!(error.to_string().contains("correlation aliases conflict"));
    }

    #[test]
    fn completed_function_and_custom_inputs_cannot_diverge_from_streamed_prefixes() {
        for (final_call, streamed_call) in [
            (
                ToolCall::function("call_1".to_string(), "search".to_string(), "abX".to_string()),
                ToolCall::function("call_1".to_string(), "search".to_string(), "abc".to_string()),
            ),
            (
                ToolCall::custom("call_2".to_string(), "patch".to_string(), "abX".to_string()),
                ToolCall::custom("call_2".to_string(), "patch".to_string(), "abc".to_string()),
            ),
        ] {
            let mut response = LLMResponse {
                tool_calls: Some(vec![final_call.clone()]),
                ..Default::default()
            };
            let streamed = LLMResponse {
                tool_calls: Some(vec![streamed_call]),
                ..Default::default()
            };
            let error = merge_streamed_response(&mut response, streamed, "TestProvider", false)
                .expect_err("divergent completed input must fail");
            assert!(error.to_string().contains("diverges from streamed prefix"));
            assert_eq!(response.tool_calls.as_mut().and_then(|calls| calls.pop()), Some(final_call));
        }
    }

    #[test]
    fn normalized_function_call_id_remap_is_opt_in_and_retains_terminal_id() {
        let final_input = r#"{"limit":10,"query":"vtcode"}"#;
        let final_call = ToolCall::function("final-id".into(), "search".into(), final_input.into());
        let streamed_call =
            ToolCall::function("stream-id".into(), "search".into(), r#"{ "query": "vtcode", "limit": 10 }"#.into());
        let mut strict = LLMResponse {
            tool_calls: Some(vec![final_call.clone()]),
            ..Default::default()
        };
        assert!(
            reconcile_streamed_tool_calls(
                &mut strict,
                Some(std::slice::from_ref(&streamed_call)),
                "TestProvider",
                false
            )
            .is_err()
        );
        assert_eq!(strict.tool_calls, Some(vec![final_call.clone()]));

        let mut compatible = LLMResponse {
            tool_calls: Some(vec![final_call]),
            ..Default::default()
        };
        reconcile_streamed_tool_calls(
            &mut compatible,
            Some(std::slice::from_ref(&streamed_call)),
            "TestProvider",
            true,
        )
        .expect("unique semantic function call should reconcile");
        let call = compatible
            .tool_calls
            .as_ref()
            .and_then(|calls| calls.first())
            .expect("terminal call");
        assert_eq!(call.id, "final-id");
        assert_eq!(call.raw_input(), Some(final_input));

        let mut missing = LLMResponse::default();
        let error = reconcile_streamed_tool_calls(
            &mut missing,
            Some(std::slice::from_ref(&streamed_call)),
            "TestProvider",
            true,
        )
        .expect_err("terminal response must retain every streamed call");
        assert!(error.to_string().contains("do not map one-to-one"));
        assert!(missing.tool_calls.is_none());
    }

    #[test]
    fn normalized_ambiguous_remap_rejects_before_mutation() {
        let mut response = LLMResponse {
            tool_calls: Some(vec![
                ToolCall::function("final-a".into(), "same".into(), "{}".into()),
                ToolCall::function("final-b".into(), "same".into(), "{}".into()),
            ]),
            ..Default::default()
        };
        let original = response.clone();
        let streamed = [
            ToolCall::function("stream-a".into(), "same".into(), "{}".into()),
            ToolCall::function("stream-b".into(), "same".into(), "{}".into()),
        ];

        assert!(reconcile_streamed_tool_calls(&mut response, Some(&streamed), "TestProvider", true).is_err());
        assert_eq!(response, original);
    }
}

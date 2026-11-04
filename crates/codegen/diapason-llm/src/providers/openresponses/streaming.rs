//! OpenResponses streaming event types.
//!
//! This module defines the semantic streaming events used by the OpenResponses specification.
//! See <https://www.openresponses.org/specification#streaming> for details.

use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

// ============================================================================
// Streaming Event Types
// ============================================================================

/// All possible streaming event types in OpenResponses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamEventType {
    // Response lifecycle events
    #[serde(rename = "response.created")]
    ResponseCreated,
    #[serde(rename = "response.in_progress")]
    ResponseInProgress,
    #[serde(rename = "response.completed")]
    ResponseCompleted,
    #[serde(rename = "response.failed")]
    ResponseFailed,
    #[serde(rename = "response.incomplete")]
    ResponseIncomplete,

    // Output item events
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded,
    #[serde(rename = "response.output_item.done")]
    OutputItemDone,

    // Text delta events
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta,
    #[serde(rename = "response.output_text.done")]
    OutputTextDone,

    // Content part events
    #[serde(rename = "response.content_part.added")]
    ContentPartAdded,
    #[serde(rename = "response.content_part.done")]
    ContentPartDone,

    // Function call events
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgumentsDelta,
    #[serde(rename = "response.function_call_arguments.done")]
    FunctionCallArgumentsDone,

    // Reasoning events
    #[serde(rename = "response.reasoning_summary_text.delta")]
    ReasoningSummaryTextDelta,
    #[serde(rename = "response.reasoning_summary_text.done")]
    ReasoningSummaryTextDone,

    // Reasoning content events
    #[serde(rename = "response.reasoning_content.delta")]
    ReasoningContentDelta,
    #[serde(rename = "response.reasoning_content.done")]
    ReasoningContentDone,

    // Error event
    #[serde(rename = "error")]
    Error,
    /// Catch-all for unknown streaming event types added by the OpenResponses spec.
    #[serde(other)]
    Unknown,
}

/// A streaming event from the OpenResponses API.
#[derive(Debug, Clone, Serialize)]
pub struct StreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    sequence_number: u32,
    #[serde(flatten)]
    data: StreamEventData,
}

impl<'de> Deserialize<'de> for StreamEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = StreamEventWire::deserialize(deserializer)?;
        let (event_type, sequence_number, data) = wire.into_event().map_err(de::Error::custom)?;
        Ok(Self { event_type, sequence_number, data })
    }
}

/// Directly decoded fields for [`StreamEvent`].
///
/// This intentionally avoids `flatten` + `untagged`: both require Serde to
/// buffer the event object before it can decide which payload shape applies.
/// OpenResponses sends a discriminator for every event, so the fields can be
/// decoded once and converted to the matching payload without an intermediate
/// map or repeated variant attempts.
#[derive(Debug, Default)]
struct StreamEventWire {
    event_type: Option<String>,
    sequence_number: u32,
    response: Option<Value>,
    item: Option<Value>,
    output_index: Option<u32>,
    item_id: Option<String>,
    content_index: Option<u32>,
    call_id: Option<String>,
    delta: Option<String>,
    error: Option<StreamError>,
    extra: Option<serde_json::Map<String, Value>>,
}

impl StreamEventWire {
    fn into_event(mut self) -> Result<(String, u32, StreamEventData), String> {
        // `take()` moves the event type out without cloning, leaving `self`
        // whole so `into_data` can still consume it. This runs per streamed
        // event.
        let event_type = required_field(self.event_type.take(), "type", "stream event")?;
        let sequence_number = self.sequence_number;
        let data = self.into_data(&event_type)?;
        Ok((event_type, sequence_number, data))
    }

    fn into_data(self, event_type: &str) -> Result<StreamEventData, String> {
        match event_type {
            "response.created"
            | "response.in_progress"
            | "response.completed"
            | "response.failed"
            | "response.incomplete" => Ok(StreamEventData::Response(ResponseEventData { response: self.response })),
            "response.output_item.added" | "response.output_item.done" => {
                Ok(StreamEventData::OutputItem(OutputItemEventData {
                    item: self.item,
                    output_index: self.output_index,
                    item_id: self.item_id,
                }))
            }
            "response.output_text.delta"
            | "response.output_text.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done" => Ok(StreamEventData::TextDelta(TextDeltaEventData {
                delta: required_field(self.delta, "delta", event_type)?,
                item_id: self.item_id,
                output_index: self.output_index,
                content_index: self.content_index,
            })),
            "response.function_call_arguments.delta" | "response.function_call_arguments.done" => {
                Ok(StreamEventData::FunctionCallDelta(FunctionCallDeltaEventData {
                    delta: required_field(self.delta, "delta", event_type)?,
                    item_id: self.item_id,
                    output_index: self.output_index,
                    call_id: self.call_id,
                }))
            }
            "response.reasoning_content.delta" | "response.reasoning_content.done" => {
                Ok(StreamEventData::ReasoningContentDelta(ReasoningContentDeltaEventData {
                    delta: required_field(self.delta, "delta", event_type)?,
                    item_id: self.item_id,
                    output_index: self.output_index,
                }))
            }
            "error" => Ok(StreamEventData::Error(ErrorEventData {
                error: required_field(self.error, "error", event_type)?,
            })),
            _ => Ok(StreamEventData::Generic(self.into_generic_value())),
        }
    }

    fn into_generic_value(self) -> Value {
        let Self {
            response,
            item,
            output_index,
            item_id,
            content_index,
            call_id,
            delta,
            error,
            extra,
            ..
        } = self;
        let mut object = extra.unwrap_or_default();
        insert_optional(&mut object, "response", response);
        insert_optional(&mut object, "item", item);
        insert_optional(&mut object, "output_index", output_index);
        insert_optional(&mut object, "item_id", item_id);
        insert_optional(&mut object, "content_index", content_index);
        insert_optional(&mut object, "call_id", call_id);
        insert_optional(&mut object, "delta", delta);
        if let Some(error) = error {
            if let Ok(value) = serde_json::to_value(error) {
                object.insert("error".to_string(), value);
            }
        }
        Value::Object(object)
    }
}

impl<'de> Deserialize<'de> for StreamEventWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StreamEventWireVisitor;

        impl<'de> Visitor<'de> for StreamEventWireVisitor {
            type Value = StreamEventWire;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an OpenResponses streaming event object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut wire = StreamEventWire::default();
                while let Some(key) = map.next_key::<&str>()? {
                    match key {
                        "type" => wire.event_type = map.next_value()?,
                        "sequence_number" => wire.sequence_number = map.next_value()?,
                        "response" => wire.response = map.next_value()?,
                        "item" => wire.item = map.next_value()?,
                        "output_index" => wire.output_index = map.next_value()?,
                        "item_id" => wire.item_id = map.next_value()?,
                        "content_index" => wire.content_index = map.next_value()?,
                        "call_id" => wire.call_id = map.next_value()?,
                        "delta" => wire.delta = map.next_value()?,
                        "error" => wire.error = map.next_value()?,
                        _ => {
                            wire.extra
                                .get_or_insert_with(serde_json::Map::new)
                                .insert(key.to_string(), map.next_value()?);
                        }
                    }
                }
                if wire.event_type.is_none() {
                    return Err(de::Error::missing_field("type"));
                }
                Ok(wire)
            }
        }

        deserializer.deserialize_map(StreamEventWireVisitor)
    }
}

fn required_field<T>(value: Option<T>, field: &str, event_type: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("OpenResponses event {event_type:?} is missing {field:?}"))
}

fn insert_optional<T: Serialize>(object: &mut serde_json::Map<String, Value>, key: &str, value: Option<T>) {
    if let Some(value) = value
        && let Ok(value) = serde_json::to_value(value)
    {
        object.insert(key.to_string(), value);
    }
}

/// Data payload for different streaming events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StreamEventData {
    /// Response lifecycle event data.
    Response(ResponseEventData),
    /// Output item event data.
    OutputItem(OutputItemEventData),
    /// Text delta event data.
    TextDelta(TextDeltaEventData),
    /// Function call arguments delta.
    FunctionCallDelta(FunctionCallDeltaEventData),
    /// Reasoning content delta.
    ReasoningContentDelta(ReasoningContentDeltaEventData),
    /// Error event data.
    Error(ErrorEventData),
    /// Generic/unknown event data.
    Generic(Value),
}

/// Data for response lifecycle events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEventData {
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<Value>,
}

/// Data for output item events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputItemEventData {
    #[serde(skip_serializing_if = "Option::is_none")]
    item: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    item_id: Option<String>,
}

/// Data for text delta events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDeltaEventData {
    delta: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_index: Option<u32>,
}

/// Data for function call argument delta events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCallDeltaEventData {
    delta: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    call_id: Option<String>,
}

/// Data for reasoning content delta events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningContentDeltaEventData {
    delta: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_index: Option<u32>,
}

/// Data for error events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEventData {
    error: StreamError,
}

/// Error details in streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamError {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    param: Option<String>,
}

// ============================================================================
// Stream Parsing Utilities
// ============================================================================

/// Parse a Server-Sent Events (SSE) line into a stream event.
fn parse_sse_event(line: &str) -> Option<StreamEvent> {
    // SSE format: "data: {...}"
    let line = line.trim();
    if line.is_empty() || line == "[DONE]" {
        return None;
    }

    if let Some(data) = line.strip_prefix("data: ") {
        if data == "[DONE]" {
            return None;
        }
        serde_json::from_str(data).ok()
    } else if line.starts_with('{') {
        // Some implementations send raw JSON
        serde_json::from_str(line).ok()
    } else {
        None
    }
}

/// Extract the event type from an SSE event line.
pub fn extract_event_type(line: &str) -> Option<String> {
    let line = line.trim();
    line.strip_prefix("event: ").map(|event_type| event_type.to_string())
}

/// Accumulator for building responses from streaming events.
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    text_content: String,
    reasoning_content: String,
    reasoning_summary: String,
    function_calls: Vec<AccumulatedFunctionCall>,
    current_function_call: Option<AccumulatingFunctionCall>,
    output_items: Vec<Value>,
    response_id: Option<String>,
    model: Option<String>,
    usage: Option<Value>,
    is_complete: bool,
    error: Option<StreamError>,
}

/// A function call being accumulated from streaming deltas.
#[derive(Debug, Clone, Default)]
pub struct AccumulatingFunctionCall {
    id: String,
    call_id: String,
    name: String,
    arguments: String,
}

/// A completed accumulated function call.
#[derive(Debug, Clone)]
pub struct AccumulatedFunctionCall {
    id: String,
    call_id: String,
    name: String,
    arguments: String,
}

impl StreamAccumulator {
    fn new() -> Self {
        Self::default()
    }

    /// Process a streaming event and update the accumulator state.
    fn process_event(&mut self, event: &StreamEvent) {
        match event.event_type.as_str() {
            "response.created" | "response.in_progress" => {
                if let StreamEventData::Response(data) = &event.data
                    && let Some(response) = &data.response
                {
                    self.response_id = response.get("id").and_then(|v| v.as_str()).map(String::from);
                    self.model = response.get("model").and_then(|v| v.as_str()).map(String::from);
                }
            }
            "response.output_text.delta" => {
                if let StreamEventData::TextDelta(data) = &event.data {
                    self.text_content.push_str(&data.delta);
                }
            }
            "response.reasoning_summary_text.delta" => {
                // Summary reasoning (sanitized version)
                if let StreamEventData::TextDelta(data) = &event.data {
                    self.reasoning_summary.push_str(&data.delta);
                }
            }
            "response.reasoning_content.delta" => {
                // Raw reasoning traces (preferred over summary)
                if let StreamEventData::ReasoningContentDelta(data) = &event.data {
                    self.reasoning_content.push_str(&data.delta);
                }
            }
            "response.function_call_arguments.delta" => {
                if let StreamEventData::FunctionCallDelta(data) = &event.data
                    && let Some(ref mut fc) = self.current_function_call
                {
                    fc.arguments.push_str(&data.delta);
                }
            }
            "response.output_item.added" => {
                if let StreamEventData::OutputItem(data) = &event.data
                    && let Some(item) = &data.item
                {
                    // Check if this is a function call item
                    if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                        let fc = AccumulatingFunctionCall {
                            id: item.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                            call_id: item.get("call_id").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                            name: item.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                            arguments: String::new(),
                        };
                        self.current_function_call = Some(fc);
                    }
                    self.output_items.push(item.clone());
                }
            }
            "response.output_item.done" => {
                // Finalize current function call if any
                if let Some(fc) = self.current_function_call.take() {
                    self.function_calls.push(AccumulatedFunctionCall {
                        id: fc.id,
                        call_id: fc.call_id,
                        name: fc.name,
                        arguments: fc.arguments,
                    });
                }
            }
            "response.completed" => {
                self.is_complete = true;
                if let StreamEventData::Response(data) = &event.data
                    && let Some(response) = &data.response
                {
                    self.usage = response.get("usage").cloned();
                }
            }
            "response.failed" => {
                self.is_complete = true;
            }
            "error" => {
                if let StreamEventData::Error(data) = &event.data {
                    self.error = Some(data.error.clone());
                }
                self.is_complete = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sse_text_delta() {
        let line = r#"data: {"type":"response.output_text.delta","sequence_number":1,"delta":"Hello"}"#;
        let event = parse_sse_event(line).unwrap();
        assert_eq!(event.event_type, "response.output_text.delta");
        assert!(matches!(
            event.data,
            StreamEventData::TextDelta(TextDeltaEventData { delta, .. }) if delta == "Hello"
        ));
    }

    #[test]
    fn test_parse_sse_dispatches_payload_by_event_type() {
        let cases = [
            (r#"data: {"type":"response.created","response":{"id":"resp_1"}}"#, "response"),
            (r#"data: {"type":"response.output_item.added","item":{"type":"message"}}"#, "output_item"),
            (
                r#"data: {"type":"response.function_call_arguments.delta","delta":"{}","call_id":"call_1"}"#,
                "function_call",
            ),
            (r#"data: {"type":"response.reasoning_content.delta","delta":"think"}"#, "reasoning"),
            (r#"data: {"type":"error","error":{"code":"bad_request","message":"nope"}}"#, "error"),
        ];

        for (line, expected) in cases {
            let event = parse_sse_event(line).expect("valid streaming event");
            let actual = match event.data {
                StreamEventData::Response(_) => "response",
                StreamEventData::OutputItem(_) => "output_item",
                StreamEventData::FunctionCallDelta(_) => "function_call",
                StreamEventData::ReasoningContentDelta(_) => "reasoning",
                StreamEventData::Error(_) => "error",
                _ => "other",
            };
            assert_eq!(actual, expected, "event line: {line}");
        }
    }

    #[test]
    fn test_parse_sse_preserves_unknown_payload_fields() {
        let line = r#"data: {"type":"response.future_event","sequence_number":7,"custom":{"value":true}}"#;
        let event = parse_sse_event(line).expect("valid unknown streaming event");
        assert_eq!(event.event_type, "response.future_event");
        assert_eq!(event.sequence_number, 7);
        assert!(matches!(
            event.data,
            StreamEventData::Generic(Value::Object(ref object))
                if object.get("custom") == Some(&serde_json::json!({"value": true}))
        ));
    }

    #[test]
    fn test_parse_done_signal() {
        assert!(parse_sse_event("[DONE]").is_none());
        assert!(parse_sse_event("data: [DONE]").is_none());
    }

    #[test]
    fn test_stream_accumulator_text() {
        let mut acc = StreamAccumulator::new();

        let event1 = StreamEvent {
            event_type: "response.output_text.delta".to_string(),
            sequence_number: 1,
            data: StreamEventData::TextDelta(TextDeltaEventData {
                delta: "Hello, ".to_string(),
                item_id: None,
                output_index: None,
                content_index: None,
            }),
        };

        let event2 = StreamEvent {
            event_type: "response.output_text.delta".to_string(),
            sequence_number: 2,
            data: StreamEventData::TextDelta(TextDeltaEventData {
                delta: "world!".to_string(),
                item_id: None,
                output_index: None,
                content_index: None,
            }),
        };

        acc.process_event(&event1);
        acc.process_event(&event2);

        assert_eq!(acc.text_content, "Hello, world!");
    }
}

//! External consumer of the tool-progress stream-event variants.

use vtcode_llm::provider::{LLMStreamEvent, NormalizedStreamEvent};

/// Label a provider stream event by variant.
fn stream_event_label(event: &LLMStreamEvent) -> &'static str {
    match event {
        LLMStreamEvent::Token { .. } => "token",
        LLMStreamEvent::Reasoning { .. } => "reasoning",
        LLMStreamEvent::ReasoningSignature { .. } => "reasoning_signature",
        LLMStreamEvent::ReasoningStage { .. } => "reasoning_stage",
        LLMStreamEvent::ToolCallStart { .. } => "tool_call_start",
        LLMStreamEvent::ToolCallDelta { .. } => "tool_call_delta",
        LLMStreamEvent::Completed { .. } => "completed",
    }
}

/// Label a normalized stream event by variant.
fn normalized_event_label(event: &NormalizedStreamEvent) -> &'static str {
    match event {
        NormalizedStreamEvent::TextDelta { .. } => "text_delta",
        NormalizedStreamEvent::ReasoningDelta { .. } => "reasoning_delta",
        NormalizedStreamEvent::ReasoningStage { .. } => "reasoning_stage",
        NormalizedStreamEvent::ToolCallStart { .. } => "tool_call_start",
        NormalizedStreamEvent::ToolCallDelta { .. } => "tool_call_delta",
        NormalizedStreamEvent::Usage { .. } => "usage",
        NormalizedStreamEvent::Done { .. } => "done",
    }
}

/// Construct and match both tool-progress variants as an external consumer.
fn main() {
    let start = LLMStreamEvent::ToolCallStart {
        call_id: "call_1".to_string(),
        name: Some("read_file".to_string()),
    };
    let delta = LLMStreamEvent::ToolCallDelta {
        call_id: "call_1".to_string(),
        delta: "{\"path\":".to_string(),
    };
    assert_eq!(stream_event_label(&start), "tool_call_start");
    assert_eq!(stream_event_label(&delta), "tool_call_delta");

    let normalized_start = NormalizedStreamEvent::ToolCallStart {
        call_id: "call_1".to_string(),
        name: None,
    };
    let normalized_delta = NormalizedStreamEvent::ToolCallDelta {
        call_id: "call_1".to_string(),
        delta: "}".to_string(),
    };
    assert_eq!(normalized_event_label(&normalized_start), "tool_call_start");
    assert_eq!(normalized_event_label(&normalized_delta), "tool_call_delta");
}

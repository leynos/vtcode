//! Tracks assistant output produced during a prompt turn.
use super::*;

pub(in crate::zed::agent::handlers) struct ProviderOutput {
    // Fields stay visible to the `prompt` subtree, matching the module the
    // type was extracted from: buffered, streaming and cancellation paths all
    // read and write them directly.
    pub(super) assistant_message: String,
    pub(super) assistant_reasoning: String,
    pub(super) emitted_output: bool,
    pub(super) assistant_emitted: bool,
}

impl ProviderOutput {
    pub(super) fn empty() -> Self {
        Self {
            assistant_message: String::new(),
            assistant_reasoning: String::new(),
            emitted_output: false,
            assistant_emitted: false,
        }
    }

    pub(super) fn take_from(turn: &mut PromptExecution) -> Self {
        let assistant_message = std::mem::take(&mut turn.assistant_message);
        let assistant_reasoning = std::mem::take(&mut turn.assistant_reasoning);
        let emitted_output = !assistant_message.is_empty() || !assistant_reasoning.is_empty();
        Self {
            assistant_message,
            assistant_reasoning,
            emitted_output,
            assistant_emitted: turn.assistant_emitted,
        }
    }

    pub(super) fn reset(&mut self) {
        self.assistant_message.clear();
        self.assistant_reasoning.clear();
        self.emitted_output = false;
        // `assistant_emitted` is turn-scoped: text streamed before a tool round
        // still reached the client and must not be forgotten.
    }

    pub(super) fn store_in(self, turn: &mut PromptExecution) {
        turn.assistant_message = self.assistant_message;
        turn.assistant_reasoning = self.assistant_reasoning;
        turn.assistant_emitted |= self.assistant_emitted;
    }

    /// Marks a buffered draft as client-visible when it has not been emitted
    /// yet, returning the text the caller must send. Cancellation flushes the
    /// draft so a partially generated answer is not silently discarded.
    pub(super) fn take_unemitted_assistant_text(&mut self) -> Option<String> {
        if self.assistant_emitted || self.assistant_message.is_empty() {
            return None;
        }
        self.assistant_emitted = true;
        Some(self.assistant_message.clone())
    }
}

pub(in crate::zed::agent::handlers) fn assistant_tool_call_message(
    content: String,
    tool_calls: Vec<vtcode_core::llm::provider::ToolCall>,
    reasoning: &str,
    reasoning_details: Option<Vec<String>>,
) -> Message {
    let reasoning_details = reasoning_details.map(|details| {
        details
            .into_iter()
            .map(|detail| match serde_json::from_str::<serde_json::Value>(&detail) {
                Ok(value) if value.is_object() || value.is_array() => value,
                _ => serde_json::Value::String(detail),
            })
            .collect()
    });
    let mut message = Message::assistant_with_tools_and_reasoning(content, tool_calls, reasoning_details);
    if !reasoning.is_empty() {
        message.reasoning = Some(reasoning.to_owned());
    }
    message
}

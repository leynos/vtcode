//! Buffered provider output for one ACP prompt turn.

use super::*;

/// Provider text buffered during a turn, with its emission marker.
pub(in crate::zed::agent::handlers) struct ProviderOutput {
    pub(super) assistant_message: String,
    pub(super) assistant_reasoning: String,
    pub(super) emitted_output: bool,
}

impl ProviderOutput {
    /// Create an output buffer with no visible text.
    pub(super) fn empty() -> Self {
        Self {
            assistant_message: String::new(),
            assistant_reasoning: String::new(),
            emitted_output: false,
        }
    }

    /// Move the turn's buffered output into a new provider output.
    pub(super) fn take_from(turn: &mut PromptExecution) -> Self {
        let assistant_message = std::mem::take(&mut turn.assistant_message);
        let assistant_reasoning = std::mem::take(&mut turn.assistant_reasoning);
        let emitted_output = !assistant_message.is_empty() || !assistant_reasoning.is_empty();
        Self {
            assistant_message,
            assistant_reasoning,
            emitted_output,
        }
    }

    /// Clear buffered text and discard any emission marker.
    pub(super) fn reset(&mut self) {
        self.assistant_message.clear();
        self.assistant_reasoning.clear();
        self.emitted_output = false;
    }

    /// Copy this output back into the turn's buffers.
    pub(super) fn store_in(self, turn: &mut PromptExecution) {
        turn.assistant_message = self.assistant_message;
        turn.assistant_reasoning = self.assistant_reasoning;
    }
}

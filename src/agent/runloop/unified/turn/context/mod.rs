//! Agent Legibility:
//! - Entrypoint: `PreparedAssistantToolCall`, `TurnLoopResult`, and the turn-context builders in this root control tool-call preparation and history shaping.
//! - Common changes:
//!   - Interim progress suppression and continuation heuristics live in `context/continuation.rs`.
//!   - Turn-processing state and response handling live in `context/runtime_context.rs` and `context/response_handling.rs`.
//! - Constraints: TD-005 is active for this hotspot; prefer extracting focused support modules over growing this root further.
//! - Verify: `cargo check -p vtcode && cargo test -p vtcode --bin vtcode turn::context`

mod continuation;
mod message_history;
mod response_handling;
mod runtime_context;

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Notify, RwLock};
use vtcode_core::config::loader::VTCodeConfig;
use vtcode_core::core::agent::runtime::RuntimeSteering;
use vtcode_core::core::agent::snapshots::SnapshotManager;
use vtcode_core::hooks::{LifecycleHookEngine, SessionEndReason};
use vtcode_core::llm::provider as uni;
use vtcode_core::llm::providers::ReasoningSegment;
use vtcode_core::tools::registry::ToolExecutionError;
use vtcode_core::utils::ansi::AnsiRenderer;
use vtcode_ui::tui::app::InlineHandle;

use self::continuation::{
    AUTONOMOUS_CONTINUE_DIRECTIVE, InterimTextContinuationDecision, evaluate_interim_text_continuation,
    push_system_directive_once,
};
use self::message_history::{
    build_combined_reasoning, parse_reasoning_detail_value, push_assistant_message, reasoning_duplicates_content,
    should_suppress_redundant_diff_recap,
};
#[cfg(test)]
use self::response_handling::looks_like_clarifying_question;
pub(crate) use self::runtime_context::{
    LLMContext, ToolContext, TurnHandlerOutcome, TurnOutcomeContext, TurnProcessingContext, TurnProcessingContextParts,
    TurnProcessingResult, TurnProcessingState, UIContext,
};
use crate::agent::runloop::mcp_events;
use crate::agent::runloop::unified::run_loop_context::{RecoveryMode, RunLoopContext};
use crate::agent::runloop::unified::state::{CtrlCState, SessionStats};
use crate::agent::runloop::unified::tool_catalogue::ToolCatalogueState;

#[derive(Clone, Debug)]
pub(crate) enum TurnLoopResult {
    Completed {
        #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
        plan_approved_execution_pending: bool,
    },
    Aborted,
    Cancelled,
    Exit,
    Blocked {
        reason: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedAssistantToolCall {
    raw_call: uni::ToolCall,
    parsed_args: Option<serde_json::Value>,
    args_error: Option<String>,
    is_parallel_safe: bool,
    is_command_execution: bool,
}

impl PreparedAssistantToolCall {
    pub(crate) fn new(raw_call: uni::ToolCall) -> Self {
        let tool_name = raw_call.tool_name().unwrap_or(raw_call.call_type.as_str());

        let (parsed_args, args_error, is_parallel_safe, is_command_execution) = if raw_call.function.is_none() {
            (None, Some("tool call missing function details".to_string()), false, false)
        } else {
            match raw_call.execution_arguments() {
                Ok(args) => {
                    let is_parallel_safe = !raw_call.is_custom()
                        && vtcode_core::tools::tool_intent::is_parallel_safe_call(tool_name, &args);
                    let is_command_execution = !raw_call.is_custom()
                        && vtcode_core::tools::tool_intent::is_command_run_tool_call(tool_name, &args);
                    (Some(args), None, is_parallel_safe, is_command_execution)
                }
                Err(err) => (None, Some(err.to_string()), false, false),
            }
        };

        Self {
            raw_call,
            parsed_args,
            args_error,
            is_parallel_safe,
            is_command_execution,
        }
    }

    pub(crate) fn raw_call(&self) -> &uni::ToolCall {
        &self.raw_call
    }

    pub(crate) fn into_raw_call(self) -> uni::ToolCall {
        self.raw_call
    }

    pub(crate) fn call_id(&self) -> &str {
        &self.raw_call.id
    }

    pub(crate) fn tool_name(&self) -> &str {
        self.raw_call
            .function
            .as_ref()
            .map(|function| function.name.as_str())
            .unwrap_or(self.raw_call.call_type.as_str())
    }

    pub(crate) fn args(&self) -> Option<&serde_json::Value> {
        self.parsed_args.as_ref()
    }

    pub(crate) fn args_error(&self) -> Option<&str> {
        self.args_error.as_deref()
    }

    pub(crate) fn is_parallel_safe(&self) -> bool {
        self.is_parallel_safe
    }

    pub(crate) fn is_command_execution(&self) -> bool {
        self.is_command_execution
    }
}

impl<'a> TurnProcessingContext<'a> {
    pub(crate) fn is_planning_active(&self) -> bool {
        self.tool_registry.is_planning_active()
    }

    pub(crate) fn is_approved_plan_execution(&self) -> bool {
        self.harness_state.is_approved_plan_execution()
    }

    pub(crate) fn set_phase(&mut self, phase: crate::agent::runloop::unified::run_loop_context::TurnPhase) {
        self.harness_state.set_phase(phase);
    }

    pub(crate) fn restore_input_status(&mut self, left: Option<String>, right: Option<String>) {
        self.handle.set_input_status(left.clone(), right.clone());
        self.input_status_state.left = left;
        self.input_status_state.right = right;
        self.input_status_state.input_status_sync_pending = false;
    }

    pub(crate) fn reset_input_to_default_placeholder(&mut self) {
        crate::agent::runloop::unified::display::reset_inline_input(self.handle, self.default_placeholder.clone());
    }

    pub(crate) fn push_system_message(&mut self, content: impl Into<String>) {
        self.working_history.push(uni::Message::system(content.into()));
    }

    pub(crate) fn reset_blocked_tool_call_streak(&mut self) {
        self.harness_state.reset_blocked_tool_call_streak();
    }

    pub(crate) fn record_blocked_tool_call(&mut self) -> usize {
        self.harness_state.record_blocked_tool_call()
    }

    pub(crate) fn blocked_tool_calls(&self) -> usize {
        self.harness_state.blocked_tool_calls
    }

    pub(crate) fn record_preflight_failure(&mut self) -> usize {
        self.harness_state.record_preflight_failure()
    }

    pub(crate) fn reset_preflight_failure_streak(&mut self) {
        self.harness_state.reset_preflight_failure_streak();
    }

    pub(crate) fn activate_recovery(&mut self, reason: impl Into<String>) {
        self.harness_state.activate_recovery(reason);
    }

    pub(crate) fn activate_recovery_with_mode(&mut self, reason: impl Into<String>, mode: RecoveryMode) {
        self.harness_state.activate_recovery_with_mode(reason, mode);
    }

    pub(crate) fn is_recovery_active(&self) -> bool {
        self.harness_state.is_recovery_active()
    }

    pub(crate) fn recovery_reason(&self) -> Option<&str> {
        self.harness_state.recovery_reason()
    }

    pub(crate) fn recovery_pass_used(&self) -> bool {
        self.harness_state.recovery_pass_used()
    }

    pub(crate) fn recovery_is_tool_free(&self) -> bool {
        self.harness_state.recovery_is_tool_free()
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(crate) fn switch_to_tool_free_recovery(&mut self) -> bool {
        self.harness_state.switch_to_tool_free_recovery()
    }

    pub(crate) fn consume_recovery_pass(&mut self) -> bool {
        self.harness_state.consume_recovery_pass()
    }

    pub(crate) fn finish_recovery_pass(&mut self) -> bool {
        self.harness_state.finish_recovery_pass()
    }

    pub(crate) fn retry_recovery_pass(&mut self) -> bool {
        self.harness_state.retry_recovery_pass()
    }

    pub(crate) fn recovery_retry_count(&self) -> u8 {
        self.harness_state.recovery_retry_count()
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(crate) fn post_tool_recovery_cycles(&self) -> u8 {
        self.harness_state.post_tool_recovery_cycles()
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(crate) fn increment_post_tool_recovery_cycle(&mut self) -> u8 {
        self.harness_state.increment_post_tool_recovery_cycle()
    }

    pub(crate) fn push_tool_response<S>(&mut self, tool_call_id: S, tool_name: Option<&str>, content: String)
    where
        S: AsRef<str> + Into<String>,
    {
        let content = self.harness_state.bound_model_visible_tool_preview(tool_name, content);
        let visible_output_bytes = content.len();
        let history_update = crate::agent::runloop::unified::turn::tool_outcomes::helpers::push_tool_response(
            self.working_history,
            tool_call_id,
            tool_name,
            content,
        );
        match history_update {
            crate::agent::runloop::unified::turn::tool_outcomes::helpers::ToolResponseHistoryUpdate::Appended => {
                self.harness_state.record_model_visible_output_append(visible_output_bytes);
            }
            crate::agent::runloop::unified::turn::tool_outcomes::helpers::ToolResponseHistoryUpdate::Replaced {
                previous_text_len,
            } => {
                self.harness_state
                    .replace_model_visible_output_bytes(previous_text_len, visible_output_bytes);
            }
        }
    }

    pub(crate) async fn record_recovery_error(
        &self,
        scope: &str,
        error: &anyhow::Error,
        error_type: vtcode_core::core::agent::error_recovery::ErrorType,
    ) {
        let mut recovery = self.error_recovery.write().await;
        recovery.record_error(scope, format!("{error:#}"), error_type);
    }

    pub(crate) async fn record_recovery_tool_error(
        &self,
        scope: &str,
        error: &ToolExecutionError,
        error_type: vtcode_core::core::agent::error_recovery::ErrorType,
    ) {
        let mut recovery = self.error_recovery.write().await;
        recovery.record_error_with_category(scope, error.message.clone(), error_type, Some(error.category));
    }

    pub(crate) async fn turn_metadata(&mut self) -> anyhow::Result<Option<serde_json::Value>> {
        if let Some(cached) = self.turn_metadata_cache.as_ref() {
            return Ok(cached.clone());
        }

        let metadata = vtcode_core::turn_metadata::build_turn_metadata_value_with_timeout(
            &self.config.workspace,
            Duration::from_millis(250),
        )
        .await?;
        *self.turn_metadata_cache = Some(metadata.clone());
        Ok(metadata)
    }
}

#[cfg(test)]
mod tests;

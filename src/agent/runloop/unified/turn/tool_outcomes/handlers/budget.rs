use diapason_core::llm::provider as uni;

use crate::agent::runloop::unified::turn::context::TurnProcessingContext;

pub(crate) fn record_tool_call_budget_usage(ctx: &mut TurnProcessingContext<'_>) {
    ctx.harness_state.record_admitted_tool_call();
    if let Some(warning) = ctx.harness_state.record_tool_call_with_default_warning() {
        ctx.working_history.push(uni::Message::system(warning.system_message()));
    }
}

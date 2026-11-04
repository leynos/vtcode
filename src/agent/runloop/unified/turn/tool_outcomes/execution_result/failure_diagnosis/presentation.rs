//! Tool-response attachment and user-facing diagnosis emission.

use diapason_core::utils::ansi::MessageStyle;
use serde_json::json;

use super::ToolFailureDiagnosis;
use super::evidence::bounded_field;
use crate::agent::runloop::unified::turn::context::TurnProcessingContext;

pub(super) fn attach_to_serialized_tool_response(serialized: String, diagnosis: &ToolFailureDiagnosis) -> String {
    let mut payload =
        serde_json::from_str::<serde_json::Value>(&serialized).unwrap_or_else(|_| json!({ "output": serialized }));
    if let Some(object) = payload.as_object_mut() {
        object.insert("diagnosis".to_string(), diagnosis.to_value());
    } else {
        payload = json!({ "output": payload, "diagnosis": diagnosis.to_value() });
    }
    serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string())
}

pub(crate) async fn push_tool_response_with_diagnosis(
    t_ctx: &mut super::super::super::handlers::ToolOutcomeContext<'_, '_>,
    tool_call_id: String,
    tool_name: &str,
    content_for_model: String,
    diagnosis: &ToolFailureDiagnosis,
) -> anyhow::Result<()> {
    let diagnosed_content = attach_to_serialized_tool_response(content_for_model, diagnosis);
    super::super::auto_permission_probe::push_tool_response_with_auto_permission_probe(
        t_ctx,
        tool_call_id,
        tool_name,
        diagnosed_content,
    )
    .await
}

pub(crate) fn render_diagnosis(
    renderer: &mut diapason_core::utils::ansi::AnsiRenderer,
    harness_emitter: Option<&crate::agent::runloop::unified::inline_events::harness::HarnessEventEmitter>,
    turn_id: &str,
    tool_name: &str,
    diagnosis: &ToolFailureDiagnosis,
) {
    let display_tool_name = bounded_field(tool_name);
    let lines = [
        format!("Diagnosis: {display_tool_name}"),
        format!("  Observed: {}", diagnosis.observed),
        format!("  Likely cause: {}", diagnosis.likely_cause),
        format!("  Next action: {}", diagnosis.next_action),
    ];
    for line in lines {
        if let Err(error) = renderer.line(MessageStyle::Info, &line) {
            tracing::warn!(tool = %display_tool_name, error = %error, "failed to render tool failure diagnosis");
            break;
        }
    }

    if let Some(emitter) = harness_emitter
        && let Err(error) = emitter.emit_diagnosis(turn_id, &diagnosis.render_text(tool_name))
    {
        tracing::warn!(tool = %display_tool_name, error = %error, "failed to emit tool failure diagnosis event");
    }
}

pub(crate) fn render_and_emit(ctx: &mut TurnProcessingContext<'_>, tool_name: &str, diagnosis: &ToolFailureDiagnosis) {
    render_diagnosis(ctx.renderer, ctx.harness_emitter, &ctx.harness_state.turn_id.0, tool_name, diagnosis);
}

//! Turn-loop completion notification helpers.

use anyhow::Result;
use diapason_core::config::loader::DiapasonConfig;
use diapason_core::llm::provider as uni;
use diapason_core::notifications::{CompletionStatus, NotificationEvent, send_global_notification};

use super::HarnessTurnState;
use crate::agent::runloop::unified::turn::context::TurnLoopResult;

pub(super) async fn emit_turn_outcome_notification(
    vt_cfg: Option<&DiapasonConfig>,
    working_history: &[uni::Message],
    workspace: &std::path::Path,
    harness_state: &HarnessTurnState,
    result: &TurnLoopResult,
) {
    let event = match result {
        TurnLoopResult::Completed { .. } => Some(NotificationEvent::Completion {
            task: "turn".to_string(),
            status: CompletionStatus::Success,
            details: None,
        }),
        TurnLoopResult::Blocked { reason } => Some(NotificationEvent::Completion {
            task: "turn".to_string(),
            status: CompletionStatus::PartialSuccess,
            details: reason.clone(),
        }),
        TurnLoopResult::Aborted => Some(NotificationEvent::Completion {
            task: "turn".to_string(),
            status: CompletionStatus::Failure,
            details: Some("Turn aborted".to_string()),
        }),
        TurnLoopResult::Cancelled => Some(NotificationEvent::Completion {
            task: "turn".to_string(),
            status: CompletionStatus::Cancelled,
            details: Some("Turn cancelled".to_string()),
        }),
        TurnLoopResult::Exit => None,
    };

    if let Some(notification) = event
        && let Err(err) = send_global_notification(notification).await
    {
        tracing::debug!(error = %err, "Failed to emit turn outcome notification");
    }

    if let Err(err) =
        maybe_run_external_turn_complete_notify(vt_cfg, working_history, workspace, harness_state, result).await
    {
        tracing::debug!(error = %err, "Failed to run external turn-complete notify hook");
    }
}

async fn maybe_run_external_turn_complete_notify(
    vt_cfg: Option<&DiapasonConfig>,
    working_history: &[uni::Message],
    workspace: &std::path::Path,
    harness_state: &HarnessTurnState,
    result: &TurnLoopResult,
) -> Result<()> {
    let Some(cfg) = vt_cfg else {
        return Ok(());
    };
    if cfg.notify.is_empty() {
        return Ok(());
    }

    let status = match result {
        TurnLoopResult::Completed { .. } => "success",
        TurnLoopResult::Blocked { .. } => "partial_success",
        TurnLoopResult::Aborted => "failure",
        TurnLoopResult::Cancelled => "cancelled",
        TurnLoopResult::Exit => return Ok(()),
    };
    // Take the 4 newest non-empty user messages (rev → take) then restore
    // chronological order with an in-place `reverse()` — one allocation
    // instead of two (the prior `collect` → `into_iter().rev().collect()`
    // allocated a second Vec just to reverse).
    let mut input_messages: Vec<String> = working_history
        .iter()
        .filter(|message| matches!(message.role, uni::MessageRole::User))
        .filter_map(|message| {
            let text = message.content.as_text();
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
        .rev()
        .take(4)
        .collect();
    input_messages.reverse();
    let last_assistant_message = working_history
        .iter()
        .rev()
        .find(|message| matches!(message.role, uni::MessageRole::Assistant))
        .map(|message| message.content.as_text().trim().to_string())
        .filter(|text| !text.is_empty());
    let payload = serde_json::json!({
        "type": "agent-turn-complete",
        "thread-id": harness_state.run_id.0,
        "turn-id": harness_state.turn_id.0,
        "cwd": workspace.display().to_string(),
        "status": status,
        "input-messages": input_messages,
        "last-assistant-message": last_assistant_message,
    });
    let payload = serde_json::to_string(&payload)?;
    let (program, args) = cfg
        .notify
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("notify command must include a program"))?;
    let status = tokio::process::Command::new(program).args(args).arg(payload).status().await?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!("notify command exited with status {status}"))
    }
}

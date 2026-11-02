//! Classification of finished prompts into the bounded outcome vocabulary.

use super::*;

/// Classifies a finished prompt into the closed outcome vocabulary.
///
/// The two sanitized failure reasons ACP produces itself are reported
/// separately; every other error collapses to `failed`.
pub(in super::super) fn prompt_outcome(result: &Result<PromptResponse, SdkError>) -> PromptOutcome {
    match result {
        Ok(response) => match response.stop_reason {
            acp::StopReason::EndTurn => PromptOutcome::EndTurn,
            acp::StopReason::MaxTokens => PromptOutcome::MaxTokens,
            acp::StopReason::MaxTurnRequests => PromptOutcome::MaxTurnRequests,
            acp::StopReason::Refusal => PromptOutcome::Refusal,
            acp::StopReason::Cancelled => PromptOutcome::Cancelled,
            _ => PromptOutcome::Failed,
        },
        Err(error) => error_outcome(error),
    }
}

/// Classifies a prompt that failed before producing a response.
pub(super) fn error_outcome(error: &SdkError) -> PromptOutcome {
    match error
        .data
        .as_ref()
        .and_then(|data| data.get("reason"))
        .and_then(serde_json::Value::as_str)
    {
        Some("empty_response") => PromptOutcome::EmptyResponse,
        Some("tool_execution_failed") => PromptOutcome::ToolExecutionFailed,
        _ => PromptOutcome::Failed,
    }
}

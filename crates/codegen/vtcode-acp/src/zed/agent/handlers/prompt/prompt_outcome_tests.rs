//! Classification of finished prompts into the bounded outcome vocabulary.
//!
//! The outcome metric must agree with the response the client received, so
//! these cases pin every stop reason and the two sanitized failure reasons.

use super::*;

/// Builds the internal error ACP returns for a sanitized failure reason.
fn prompt_error(reason: &str) -> SdkError {
    SdkError::internal_error().data(json!({ "reason": reason }))
}

#[test]
fn stop_reasons_map_to_their_own_outcomes() {
    for (reason, expected) in [
        (acp::StopReason::EndTurn, PromptOutcome::EndTurn),
        (acp::StopReason::MaxTokens, PromptOutcome::MaxTokens),
        (acp::StopReason::MaxTurnRequests, PromptOutcome::MaxTurnRequests),
        (acp::StopReason::Refusal, PromptOutcome::Refusal),
        (acp::StopReason::Cancelled, PromptOutcome::Cancelled),
    ] {
        let result = Ok(PromptResponse::new(reason));
        assert_eq!(prompt_outcome(&result), expected);
    }
}

#[test]
fn sanitized_failures_keep_their_own_outcome() {
    let empty: Result<PromptResponse, SdkError> = Err(prompt_error("empty_response"));
    let tool: Result<PromptResponse, SdkError> = Err(prompt_error("tool_execution_failed"));

    assert_eq!(prompt_outcome(&empty), PromptOutcome::EmptyResponse);
    assert_eq!(prompt_outcome(&tool), PromptOutcome::ToolExecutionFailed);
}

#[test]
fn unclassified_failures_collapse_to_failed() {
    let other_reason: Result<PromptResponse, SdkError> = Err(prompt_error("unknown_session"));
    let no_reason: Result<PromptResponse, SdkError> = Err(SdkError::internal_error());

    assert_eq!(prompt_outcome(&other_reason), PromptOutcome::Failed);
    assert_eq!(prompt_outcome(&no_reason), PromptOutcome::Failed);
}

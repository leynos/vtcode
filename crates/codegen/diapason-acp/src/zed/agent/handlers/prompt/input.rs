//! Resolves an incoming ACP prompt and applies user-prompt lifecycle hooks.
use super::super::super::{warn_hook_block_reason, warn_hook_message};
use super::*;
use vtcode_core::metrics::ToolRecoveryEvent;

/// Resolves the target session and claims its turn for an incoming prompt.
///
/// Returns early with a `Cancelled` stop reason when the session was closed or
/// its cancellation token is still asserted, so no turn is started for a
/// prompt that can no longer be answered.
pub(super) async fn prepare_prompt_input(
    agent: Arc<ZedAgent>,
    args: PromptRequest,
) -> Result<PromptInputOutcome, SdkError> {
    let Some(session) = agent
        .session_handle(&args.session_id)
        .map_err(|error| session_access_error(&args.session_id, "looking up session", error))?
    else {
        return Err(SdkError::invalid_params().data(json!({ "reason": "unknown_session" })));
    };

    if session.cancellation.is_closed() {
        return Ok(PromptInputOutcome::Response(PromptResponse::new(acp::StopReason::Cancelled)));
    }

    let thread = session.data.lock().map_err(|_err| SdkError::internal_error())?.thread.clone();
    let turn_guard = TurnGuard::begin(thread).inspect_err(|_| {
        agent
            .recovery_metrics()
            .record_tool_recovery(ToolRecoveryEvent::ConcurrentPromptRejection);
    })?;

    if let Err(error) = agent.replay_persisted_task_plan(&session, &args.session_id).await {
        warn!(%error, session_id = %args.session_id, "Failed to replay persisted ACP task plan");
    }

    start_prompt_safety_budget(&agent, &session, &args.session_id)?;
    if !session.cancellation.reset() {
        return Ok(PromptInputOutcome::Response(PromptResponse::new(acp::StopReason::Cancelled)));
    }

    let user_message = tokio::select! {
        () = session.cancellation.cancelled() => {
            return Ok(PromptInputOutcome::Response(PromptResponse::new(acp::StopReason::Cancelled)));
        }
        result = agent.resolve_prompt(&session, &args.session_id, &args.prompt) => result?,
    };

    Ok(PromptInputOutcome::Ready(PromptInput {
        agent,
        args,
        session,
        _turn_guard: turn_guard,
        user_message,
    }))
}

fn start_prompt_safety_budget(
    agent: &ZedAgent,
    session: &SessionHandle,
    session_id: &acp::SessionId,
) -> Result<(), SdkError> {
    if let Some(runtime) = session
        .workspace_runtime()
        .map_err(|error| session_access_error(session_id, "reading workspace runtime", error))?
    {
        runtime.local_tool_registry.safety_gateway().start_turn();
    } else {
        agent.local_tool_registry.safety_gateway().start_turn();
    }
    Ok(())
}

pub(super) async fn accept_user_prompt(input: &PromptInput) -> Result<bool, SdkError> {
    let PromptInput { agent, args, session, user_message, .. } = input;
    if let Some(reason) = apply_user_prompt_hooks(input).await? {
        warn_hook_block_reason("UserPromptSubmit", &reason);
        return Ok(false);
    }

    push_session_message(agent, session, &args.session_id, Message::user(user_message.clone()))?;
    agent
        .merge_session_acp_meta(session, args.meta.clone())
        .map_err(|error| session_access_error(&args.session_id, "merging client metadata", error))?;
    persist_session_checkpoint(agent, session, "user_message").await;
    Ok(true)
}

async fn apply_user_prompt_hooks(input: &PromptInput) -> Result<Option<String>, SdkError> {
    let PromptInput { agent, args, session, user_message, .. } = input;
    let Some(hooks) = session
        .lifecycle_hooks()
        .map_err(|error| session_access_error(&args.session_id, "reading lifecycle hooks", error))?
    else {
        return Ok(None);
    };
    let outcome = hooks
        .run_user_prompt_submit(&args.session_id.to_string(), user_message)
        .await
        .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
    for message in outcome.messages {
        warn_hook_message("UserPromptSubmit", &message);
    }
    for context in outcome.additional_context {
        if !context.trim().is_empty() {
            push_session_message(agent, session, &args.session_id, Message::system(context))?;
        }
    }
    if outcome.allow_prompt {
        Ok(None)
    } else {
        Ok(Some(
            outcome
                .block_reason
                .unwrap_or_else(|| "Prompt blocked by lifecycle hook".to_string()),
        ))
    }
}

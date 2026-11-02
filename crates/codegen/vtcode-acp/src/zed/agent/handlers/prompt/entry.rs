//! Prompt admission: the per-session gate, bounded scheduling metrics, and the
//! pre-initialization exits that still record one outcome.

use super::*;

/// Milliseconds between two samples of the prompt-gate clock.
///
/// Saturates instead of wrapping so a pathological wait stays observable.
pub(in super::super) fn gate_wait_ms(started: Instant, finished: Instant) -> u64 {
    u64::try_from(finished.saturating_duration_since(started).as_millis()).unwrap_or(u64::MAX)
}

/// Runs a prompt with production clocks and metrics.
pub(in super::super) async fn run_prompt(
    agent: Arc<ZedAgent>,
    args: PromptRequest,
) -> Result<PromptResponse, SdkError> {
    run_prompt_with_retry_observer(agent, args, SystemTime::now, Instant::now, ProviderMetricSink::production()).await
}

/// Test entry point that substitutes the provider metric sink.
#[cfg(test)]
pub(in super::super) async fn run_prompt_with_metric_sink(
    agent: Arc<ZedAgent>,
    args: PromptRequest,
    metric_sink: ProviderMetricSink,
) -> Result<PromptResponse, SdkError> {
    run_prompt_with_retry_observer(agent, args, SystemTime::now, Instant::now, metric_sink).await
}

/// Prepares a prompt and runs its provider turn behind the per-session gate.
///
/// `retry_observed_at` supplies provider retry timestamps and `gate_clock`
/// samples gate-queue waits, so tests can make both deterministic.
pub(in super::super) async fn run_prompt_with_retry_observer(
    agent: Arc<ZedAgent>,
    args: PromptRequest,
    retry_observed_at: impl Fn() -> SystemTime + Send + Sync,
    gate_clock: impl Fn() -> Instant + Send + Sync,
    metric_sink: ProviderMetricSink,
) -> Result<PromptResponse, SdkError> {
    let session = agent
        .session_handle(&args.session_id)
        .map_err(|error| session_access_error(&args.session_id, "looking up session", error))?
        .ok_or_else(|| SdkError::invalid_params().data(json!({ "reason": "unknown_session" })))?;
    // ACP lets a client steer an active turn by sending another
    // `session/prompt`. Serialize the turns per session instead of rejecting
    // the steer or racing the active turn's state; this is only awaited here,
    // inside the spawned prompt task.
    //
    // Sample the admitted count before waiting: it is the queue depth this
    // prompt joined, including any turn already holding the gate. Reporting it
    // alongside the wait keeps a stalled turn visible without capping steers,
    // which ACP deliberately allows.
    let queued_prompts = session.admitted_prompt_count().saturating_sub(1);
    let gate_wait_started = gate_clock();
    if queued_prompts > 0 {
        // Emit before waiting: a turn stalled on the gate must be visible
        // while it is stalled, not only once it finally runs.
        info!(
            session_id = %args.session_id,
            queued_prompts,
            "Waiting for the ACP prompt gate"
        );
    }
    let _prompt_gate = session.acquire_prompt_gate().await;
    let wait_ms = gate_wait_ms(gate_wait_started, gate_clock());
    metric_sink
        .record_prompt(PromptMetricObservation::GateQueueDepth { depth: queued_prompts })
        .await;
    metric_sink
        .record_prompt(PromptMetricObservation::GateWait { duration: Duration::from_millis(wait_ms) })
        .await;
    debug!(
        session_id = %args.session_id,
        queued_prompts,
        wait_ms,
        "Acquired ACP prompt gate"
    );
    // Every admitted prompt records exactly one trailing outcome. A prompt
    // that finishes during admission never reaches `PromptExecution::run`, so
    // it is counted here under the fixed pre-initialization mode.
    let preflight_sink = metric_sink.clone();
    match run_admitted_prompt(agent, args, &session, &retry_observed_at, metric_sink).await {
        AdmittedPrompt::Turned(result) => result,
        AdmittedPrompt::Finished(result) => {
            preflight_sink
                .record_prompt(PromptMetricObservation::Outcome {
                    mode: ProviderMode::PreInit,
                    outcome: prompt_outcome(&result),
                })
                .await;
            result
        }
    }
}

/// Whether an admitted prompt reached its provider turn.
enum AdmittedPrompt {
    /// The provider turn ran and recorded its own outcome.
    Turned(Result<PromptResponse, SdkError>),
    /// The prompt finished during admission, before any provider turn.
    Finished(Result<PromptResponse, SdkError>),
}

/// Runs the admission steps and, when they succeed, the provider turn.
async fn run_admitted_prompt(
    agent: Arc<ZedAgent>,
    args: PromptRequest,
    session: &SessionHandle,
    retry_observed_at: &provider::RetryObservationClock<'_>,
    metric_sink: ProviderMetricSink,
) -> AdmittedPrompt {
    let prepared = match prepare_admitted_prompt(agent, args, session).await {
        Ok(prepared) => prepared,
        Err(error) => return AdmittedPrompt::Finished(Err(error)),
    };
    match prepared {
        PreparedPrompt::Execution(mut execution) => {
            execution.metric_sink = metric_sink;
            AdmittedPrompt::Turned(execution.run(retry_observed_at).await)
        }
        PreparedPrompt::Finished(response) => AdmittedPrompt::Finished(Ok(response)),
    }
}

/// The result of admission: either a provider turn to run or a final response.
///
/// The execution is boxed because it is far larger than a finished response,
/// and admission returns this enum on every prompt.
enum PreparedPrompt {
    Execution(Box<PromptExecution>),
    Finished(PromptResponse),
}

/// Clears cancellation, prepares the prompt, and initializes its execution.
async fn prepare_admitted_prompt(
    agent: Arc<ZedAgent>,
    args: PromptRequest,
    session: &SessionHandle,
) -> Result<PreparedPrompt, SdkError> {
    // Clear cancellation only once the previous turn has fully completed. A
    // queued steer must not clear a cancel aimed at the turn it waited for.
    if !session.cancellation.reset() {
        return Ok(PreparedPrompt::Finished(PromptResponse::new(acp::StopReason::Cancelled)));
    }
    let input = match input::prepare_prompt_input(agent, args).await? {
        PromptInputOutcome::Response(response) => return Ok(PreparedPrompt::Finished(response)),
        PromptInputOutcome::Ready(input) => input,
    };
    if !input::accept_user_prompt(&input).await? {
        return Ok(PreparedPrompt::Finished(PromptResponse::new(acp::StopReason::Refusal)));
    }
    let execution = initialize_prompt_execution(input).await?;
    Ok(PreparedPrompt::Execution(Box::new(execution)))
}

//! Supplies ACP prompt provider calls, stream deadlines, retry telemetry, and incomplete-turn helpers.
use super::*;
use crate::zed::provider_runtime::ProviderClass;
use crate::zed::types::SessionCancellation;
use vtcode_core::retry::RetryPolicy;

mod telemetry;
pub(super) use telemetry::{ProviderErrorTelemetry, duration_millis, provider_error_telemetry, retry_disposition};

/// A future returned when announcing a failed provider attempt.
pub(super) type AttemptFailureFuture<'a> = std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// Announces failed provider attempts to whatever observes the turn.
///
/// Retry orchestration depends on this narrow port rather than on the ACP
/// transport, so retry policy stays independent of how a notice is built and
/// delivered. The ACP adapter that implements it lives at the handler
/// boundary.
///
/// The sink travels through the retry loop, which runs inside the ACP
/// dispatch task, so it must be sharable across that task boundary: `Send`
/// because the future owning it is spawned, and `Sync` because it is held by
/// shared reference across the loop's awaits.
pub(super) trait AttemptFailureSink: Send + Sync {
    /// Announce one failed provider attempt.
    fn announce<'a>(&'a self, failure: AttemptFailure<'a>) -> AttemptFailureFuture<'a>;
}

/// One failed provider attempt, as announced to an [`AttemptFailureSink`].
///
/// `retry_delay` is `Some` when another attempt is scheduled, and
/// `observed_at` is the instant the failure was observed. `metric_sink` and
/// `provider_class` travel with the failure so the adapter can record notice
/// delivery failures without the retry loop knowing how notices are built.
pub(super) struct AttemptFailure<'a> {
    pub(super) provider: &'a str,
    pub(super) error: &'a LLMError,
    pub(super) retry_delay: Option<Duration>,
    pub(super) observed_at: SystemTime,
    pub(super) metric_sink: &'a ProviderMetricSink,
    pub(super) provider_class: ProviderClass,
}

#[cfg(test)]
pub(super) type PromptProviderFactory = dyn Fn() -> Box<dyn LLMProvider> + Send + Sync;

#[cfg(test)]
pub(super) struct PromptProviderOverride {
    pub(super) provider_name: String,
    pub(super) factory: Arc<PromptProviderFactory>,
}

#[cfg(test)]
pub(super) static PROMPT_PROVIDER_OVERRIDE: std::sync::LazyLock<std::sync::Mutex<Option<PromptProviderOverride>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// Create the provider, honouring the test override when one is installed.
pub(super) fn create_prompt_provider(
    provider_name: &str,
    config: ProviderConfig,
) -> Result<Box<dyn LLMProvider>, LLMError> {
    #[cfg(test)]
    if let Some(provider) = PROMPT_PROVIDER_OVERRIDE.lock().ok().and_then(|guard| {
        guard
            .as_ref()
            .filter(|provider_override| provider_override.provider_name == provider_name)
            .map(|provider_override| (provider_override.factory)())
    }) {
        return Ok(provider);
    }

    create_provider_with_config(provider_name, config)
}

/// Record the attempt outcome, status, and elapsed time on its span.
pub(super) fn record_stream_attempt(span: &Span, started_at: Instant, outcome: &'static str, status: Option<u16>) {
    let _ = span.record("outcome", outcome);
    if let Some(status) = status {
        let _ = span.record("status", status);
    }
    let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let _ = span.record("elapsed_ms", elapsed_ms);
}

/// Poll the next item from a provider stream inside the attempt span.
pub(super) fn poll_provider_stream<S>(stream: &mut S, span: Span) -> impl Future<Output = Option<S::Item>> + '_
where
    S: futures::Stream + Unpin,
{
    stream.next().instrument(span)
}

#[derive(Debug)]
pub(super) enum ProviderCallError {
    Cancelled,
    Failed(SafeProviderProjection),
}

impl From<ProviderAdmissionError> for ProviderCallError {
    /// Map admission cancellation and failures into provider call errors.
    fn from(error: ProviderAdmissionError) -> Self {
        match error {
            ProviderAdmissionError::Cancelled => Self::Cancelled,
            _error => Self::Failed(SafeProviderProjection::execution_error()),
        }
    }
}

/// Sleep for the retry delay unless the session is cancelled.
pub(super) async fn cancellable_backoff(
    delay: Duration,
    cancellation: &SessionCancellation,
    provider_class: ProviderClass,
) -> Result<(), ProviderCallError> {
    let retry_delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
    let span = tracing::info_span!(
        "acp_retry_wait",
        provider_class = provider_class.as_str(),
        retry_delay_ms = retry_delay_ms,
        outcome = tracing::field::Empty,
        elapsed_ms = tracing::field::Empty
    );
    let started_at = Instant::now();
    let wait = async {
        tokio::select! {
            () = cancellation.cancelled() => Err(ProviderCallError::Cancelled),
            () = sleep_until_optional(Instant::now().checked_add(delay)) => Ok(()),
        }
    };
    let result = wait.instrument(span.clone()).await;
    let _ = span.record("outcome", if result.is_ok() { "elapsed" } else { "cancelled" });
    let _ = span.record("elapsed_ms", duration_millis(started_at.elapsed()));
    result
}

/// Sleep until an optional deadline, or forever when it is absent.
pub(super) async fn sleep_until_optional(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// Compute an optional deadline from now.
pub(super) fn deadline_after(timeout: Option<Duration>) -> Option<Instant> {
    deadline_from(Instant::now(), timeout)
}

/// Compute an optional deadline from the given start instant.
pub(super) fn deadline_from(started_at: Instant, timeout: Option<Duration>) -> Option<Instant> {
    timeout.map(|timeout| started_at + timeout)
}

/// Build the network error describing an expired stream phase deadline.
pub(super) fn provider_timeout_error(provider: &str, phase: StreamTimeoutPhase, timeout: Option<Duration>) -> LLMError {
    let duration = timeout.map_or_else(|| "configured deadline".to_string(), |timeout| format!("{timeout:?}"));
    LLMError::Network {
        message: format!("provider '{provider}' exceeded its {} timeout ({duration})", phase.label()),
        metadata: Some(LLMErrorMetadata::new(
            "ACP",
            None,
            Some(phase.error_code().to_string()),
            None,
            None,
            None,
            None,
        )),
    }
}

/// Return the current Unix time in milliseconds, or zero when unavailable.
fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}

pub(super) struct IncompleteProviderTurn {
    pub(super) message: Message,
    pub(super) visible_update: String,
    pub(super) response: PromptResponse,
}

impl IncompleteProviderTurn {
/// Build an incomplete-turn message, visible notice, and response from a failure.
    pub(super) fn from_failure(content: &str, reasoning: &str, error: SafeProviderProjection) -> Self {
        let error_detail = error.client_message();
        let notice = format!("The provider could not complete this turn. You can retry the prompt.\n\n{error_detail}");
        let visible_update = if content.is_empty() {
            notice
        } else {
            format!("\n\n{notice}")
        };
        let message_content = format!("{content}{visible_update}");
        let message = incomplete_assistant_message(&message_content, reasoning, &error_detail);
        Self {
            message,
            visible_update,
            response: PromptResponse::new(acp::StopReason::EndTurn),
        }
    }
}

/// Build an assistant message marked as an incomplete response.
fn incomplete_assistant_message(content: &str, reasoning: &str, error: &str) -> Message {
    let mut message = Message::assistant(content.to_string());
    if !reasoning.is_empty() {
        message.reasoning = Some(reasoning.to_string());
    }
    message.metadata =
        Some(MessageMetadata::incomplete_llm_response(unix_timestamp_millis(), message.estimate_tokens(), error));
    message
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StreamTimeoutPhase {
    FirstToken,
    InterTokenIdle,
    TotalGeneration,
}

/// Sleep until the next stream deadline and report which phase expired.
pub(super) async fn sleep_until_stream_deadline(deadline: Option<(StreamTimeoutPhase, Instant)>) -> StreamTimeoutPhase {
    match deadline {
        Some((phase, deadline)) => {
            tokio::time::sleep_until(deadline).await;
            phase
        }
        None => std::future::pending().await,
    }
}

impl StreamTimeoutPhase {
    /// Return the human-readable name of this timeout phase.
    const fn label(self) -> &'static str {
        match self {
            Self::FirstToken => "time to first token",
            Self::InterTokenIdle => "inter-token idle",
            Self::TotalGeneration => "total generation",
        }
    }

    /// Return the stable telemetry code for this timeout phase.
    const fn error_code(self) -> &'static str {
        match self {
            Self::FirstToken => "acp_timeout_first_token",
            Self::InterTokenIdle => "acp_timeout_inter_token_idle",
            Self::TotalGeneration => "acp_timeout_total_generation",
        }
    }

    /// Return the configured timeout for this phase, if any.
    pub(super) const fn timeout(self, policy: ProviderDeadlinePolicy) -> Option<Duration> {
        match self {
            Self::FirstToken => policy.first_token,
            Self::InterTokenIdle => policy.stream_idle,
            Self::TotalGeneration => policy.total_generation,
        }
    }
}

pub(super) struct StreamDeadlineTracker {
    pub(super) policy: ProviderDeadlinePolicy,
    pub(super) first_token: Option<Instant>,
    pub(super) idle: Option<Instant>,
    pub(super) total: Option<Instant>,
}

pub(super) struct GenerationTelemetry {
    metric_sink: ProviderMetricSink,
    mode: ProviderMode,
    // Starts before calling the provider router, so first-token and total
    // latency include any wait at the router's shared admission gate.
    started_at: Instant,
    pub(super) first_output_at: Option<Instant>,
    estimated_output_tokens: u64,
    runtime: ProviderRequestRuntime,
    finished: bool,
}

pub(super) struct GenerationFailure<'a> {
    pub(super) runtime: &'a ProviderRequestRuntime,
    pub(super) retry_count: u32,
    pub(super) retry_disposition: RetryDisposition,
    pub(super) retry_delay: Option<Duration>,
    pub(super) error: &'a LLMError,
}

impl StreamDeadlineTracker {
    /// Create deadline state from a policy and start instant.
    pub(super) fn new(policy: ProviderDeadlinePolicy, started_at: Instant) -> Self {
        Self {
            policy,
            first_token: deadline_from(started_at, policy.first_token),
            idle: None,
            total: deadline_from(started_at, policy.total_generation),
        }
    }

    /// Reset first-token and idle deadlines after output at the current time.
    pub(super) fn observe_output(&mut self) {
        self.observe_output_at(Instant::now());
    }

    /// Reset first-token and idle deadlines after output observed at `observed_at`.
    pub(super) fn observe_output_at(&mut self, observed_at: Instant) {
        self.first_token = None;
        self.idle = deadline_from(observed_at, self.policy.stream_idle);
    }

    /// Return the earliest pending deadline and its phase.
    pub(super) fn next(&self) -> Option<(StreamTimeoutPhase, Instant)> {
        [
            self.first_token.map(|deadline| (StreamTimeoutPhase::FirstToken, deadline)),
            self.idle.map(|deadline| (StreamTimeoutPhase::InterTokenIdle, deadline)),
            self.total.map(|deadline| (StreamTimeoutPhase::TotalGeneration, deadline)),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(_phase, deadline)| *deadline)
    }
}

pub(super) type RetryObservationClock<'clock> = dyn Fn() -> SystemTime + Send + Sync + 'clock;

/// The provider call a retry loop owns, and where its failures are announced.
pub(super) struct RetryRequest<'a> {
    pub(super) provider: &'a dyn LLMProvider,
    pub(super) request: LLMRequest,
    pub(super) runtime: &'a ProviderRequestRuntime,
    pub(super) cancellation: &'a SessionCancellation,
    pub(super) failure_sink: Option<&'a dyn AttemptFailureSink>,
}

impl<'a> RetryRequest<'a> {
    pub(super) fn new(
        provider: &'a dyn LLMProvider,
        request: LLMRequest,
        runtime: &'a ProviderRequestRuntime,
        cancellation: &'a SessionCancellation,
    ) -> Self {
        Self {
            provider,
            request,
            runtime,
            cancellation,
            failure_sink: None,
        }
    }

    pub(super) fn with_failure_sink(mut self, failure_sink: &'a dyn AttemptFailureSink) -> Self {
        self.failure_sink = Some(failure_sink);
        self
    }
}

#[cfg(test)]
pub(super) async fn generate_with_retry(request: RetryRequest<'_>) -> Result<LLMResponse, ProviderCallError> {
    generate_with_retry_with_observer(request, &SystemTime::now).await
}

#[cfg(test)]
pub(super) async fn generate_with_retry_with_observer(
    request: RetryRequest<'_>,
    retry_observed_at: &RetryObservationClock<'_>,
) -> Result<LLMResponse, ProviderCallError> {
    generate_with_retry_with_observer_and_metric_sink(request, retry_observed_at, &ProviderMetricSink::production())
        .await
}

enum GenerateOutcome {
    /// Boxed because a full response dwarfs the other two variants.
    Completed(Box<LLMResponse>),
    Failed(LLMError),
    Cancelled,
}

/// Runs one buffered provider request under the total-generation deadline.
async fn generate_once(retry_request: &RetryRequest<'_>, attempt_index: u32) -> GenerateOutcome {
    let RetryRequest { provider, request, runtime, cancellation, .. } = retry_request;
    let deadline_policy = runtime.deadline_policy();
    let total_deadline = deadline_after(deadline_policy.total_generation);
    let span = tracing::info_span!(
        "acp_provider_generate",
        provider_class = runtime.provider_class().as_str(),
        attempt = attempt_index,
        outcome = tracing::field::Empty,
        status = tracing::field::Empty,
        elapsed_ms = tracing::field::Empty
    );
    let started_at = Instant::now();
    let outcome = tokio::select! {
        () = cancellation.cancelled() => GenerateOutcome::Cancelled,
        () = sleep_until_optional(total_deadline) => GenerateOutcome::Failed(provider_timeout_error(
            runtime.provider_name(),
            StreamTimeoutPhase::TotalGeneration,
            deadline_policy.total_generation,
        )),
        result = provider.generate(request.clone()).instrument(span.clone()) => match result {
            Ok(response) => GenerateOutcome::Completed(Box::new(response)),
            Err(error) => GenerateOutcome::Failed(error),
        },
    };
    let _ = span.record("elapsed_ms", duration_millis(started_at.elapsed()));
    match &outcome {
        GenerateOutcome::Completed(_) => {
            let _ = span.record("outcome", "completed");
        }
        GenerateOutcome::Failed(error) => {
            let _ = span.record("outcome", "failed");
            if let Some(status) = SafeProviderProjection::from_llm_error(error).status() {
                let _ = span.record("status", status);
            }
        }
        GenerateOutcome::Cancelled => {
            let _ = span.record("outcome", "cancelled");
        }
    }
    outcome
}

fn observe_buffered_response(
    telemetry: &mut GenerationTelemetry,
    runtime: &ProviderRequestRuntime,
    response: &LLMResponse,
    attempt_index: u32,
) {
    if let Some(content) = response.content.as_deref() {
        telemetry.observe_output(runtime, content, attempt_index);
    }
    if let Some(reasoning) = response.reasoning.as_deref() {
        telemetry.observe_output(runtime, reasoning, attempt_index);
    }
}

fn log_retry_scheduled(runtime: &ProviderRequestRuntime, policy: &RetryPolicy, attempt_index: u32, delay: Duration) {
    info!(
        provider_class = runtime.provider_class().as_str(),
        next_attempt = attempt_index + 2,
        retry_count = attempt_index + 1,
        max_retries = policy.max_attempts.saturating_sub(1),
        ?delay,
        "Retrying transient ACP provider request"
    );
}

pub(super) async fn generate_with_retry_with_observer_and_metric_sink(
    retry_request: RetryRequest<'_>,
    retry_observed_at: &RetryObservationClock<'_>,
    metric_sink: &ProviderMetricSink,
) -> Result<LLMResponse, ProviderCallError> {
    let runtime = retry_request.runtime;
    let cancellation = retry_request.cancellation;
    let failure_sink = retry_request.failure_sink;
    let policy = runtime.retry_policy();
    let mut attempt_index = 0;
    let mut backoff = RetryBackoff::new();

    loop {
        let permit = runtime.acquire(cancellation).await?;
        let mut telemetry = GenerationTelemetry::start(runtime, metric_sink.clone(), ProviderMode::Buffered);
        telemetry.record_attempt(runtime).await;
        match generate_once(&retry_request, attempt_index).await {
            GenerateOutcome::Cancelled => return Err(ProviderCallError::Cancelled),
            GenerateOutcome::Completed(response) => {
                observe_buffered_response(&mut telemetry, runtime, &response, attempt_index);
                telemetry.complete(runtime, &response, attempt_index, true).await;
                drop(permit);
                return Ok(*response);
            }
            GenerateOutcome::Failed(error) => {
                let observed_at = retry_observed_at();
                let decision = policy.decision_for_llm_error_with_backoff_at(
                    &error,
                    LlmRetryAttempt::new(attempt_index, observed_at),
                    &mut backoff,
                );
                let retry_delay = decision.retryable.then(|| {
                    decision
                        .delay
                        .unwrap_or_else(|| policy.delay_for_attempt(AttemptIndex::new(attempt_index)))
                });
                telemetry
                    .failed(GenerationFailure {
                        runtime,
                        retry_count: attempt_index,
                        retry_disposition: retry_disposition(&decision),
                        retry_delay,
                        error: &error,
                    })
                    .await;
                drop(permit);
                if let Some(sink) = failure_sink {
                    sink.announce(AttemptFailure {
                        provider: runtime.provider_name(),
                        error: &error,
                        retry_delay,
                        observed_at,
                        metric_sink,
                        provider_class: runtime.provider_class(),
                    })
                    .await;
                }
                if !decision.retryable {
                    return Err(ProviderCallError::Failed(SafeProviderProjection::from_llm_error(&error)));
                }
                let delay = retry_delay.unwrap_or_else(|| policy.delay_for_attempt(AttemptIndex::new(attempt_index)));
                log_retry_scheduled(runtime, &policy, attempt_index, delay);
                cancellable_backoff(delay, cancellation, runtime.provider_class()).await?;
                attempt_index = attempt_index.saturating_add(1);
            }
        }
    }
}
// weave: run 'weave explain crates/codegen/vtcode-acp/src/zed/agent/handlers/provider.rs' for per-hunk detail, 'weave check' to verify your resolution
// weave: run 'weave explain crates/codegen/vtcode-acp/src/zed/agent/handlers/provider.rs' for per-hunk detail, 'weave check' to verify your resolution
// weave: run 'weave explain crates/codegen/vtcode-acp/src/zed/agent/handlers/provider.rs' for per-hunk detail, 'weave check' to verify your resolution

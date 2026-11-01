//! Supplies ACP prompt provider calls, stream deadlines, retry telemetry, and incomplete-turn helpers.
use super::*;
use crate::zed::types::SessionCancellation;
use vtcode_core::retry::RetryPolicy;

mod telemetry;
pub(super) use telemetry::{ProviderErrorTelemetry, duration_millis, provider_error_telemetry, retry_disposition};

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

pub(super) fn record_stream_attempt(span: &Span, started_at: Instant, outcome: &'static str) {
    let _ = span.record("outcome", outcome);
    let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let _ = span.record("elapsed_ms", elapsed_ms);
}

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
    fn from(error: ProviderAdmissionError) -> Self {
        match error {
            ProviderAdmissionError::Cancelled => Self::Cancelled,
            _error => Self::Failed(SafeProviderProjection::execution_error()),
        }
    }
}

pub(super) async fn cancellable_backoff(
    delay: Duration,
    cancellation: &SessionCancellation,
) -> Result<(), ProviderCallError> {
    let retry_delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
    let span = tracing::info_span!("acp_retry_wait", retry_delay_ms = retry_delay_ms, outcome = tracing::field::Empty);
    let wait = async {
        tokio::select! {
            () = cancellation.cancelled() => Err(ProviderCallError::Cancelled),
            () = sleep_until_optional(Instant::now().checked_add(delay)) => Ok(()),
        }
    };
    let result = wait.instrument(span.clone()).await;
    let _ = span.record("outcome", if result.is_ok() { "elapsed" } else { "cancelled" });
    result
}

pub(super) async fn sleep_until_optional(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

pub(super) fn deadline_after(timeout: Option<Duration>) -> Option<Instant> {
    deadline_from(Instant::now(), timeout)
}

pub(super) fn deadline_from(started_at: Instant, timeout: Option<Duration>) -> Option<Instant> {
    timeout.map(|timeout| started_at + timeout)
}

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
    const fn label(self) -> &'static str {
        match self {
            Self::FirstToken => "time to first token",
            Self::InterTokenIdle => "inter-token idle",
            Self::TotalGeneration => "total generation",
        }
    }

    const fn error_code(self) -> &'static str {
        match self {
            Self::FirstToken => "acp_timeout_first_token",
            Self::InterTokenIdle => "acp_timeout_inter_token_idle",
            Self::TotalGeneration => "acp_timeout_total_generation",
        }
    }

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
    pub(super) fn new(policy: ProviderDeadlinePolicy, started_at: Instant) -> Self {
        Self {
            policy,
            first_token: deadline_from(started_at, policy.first_token),
            idle: None,
            total: deadline_from(started_at, policy.total_generation),
        }
    }

    pub(super) fn observe_output(&mut self) {
        self.observe_output_at(Instant::now());
    }

    pub(super) fn observe_output_at(&mut self, observed_at: Instant) {
        self.first_token = None;
        self.idle = deadline_from(observed_at, self.policy.stream_idle);
    }

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

/// The provider call a retry loop owns, and where its notices are published.
pub(super) struct RetryRequest<'a> {
    pub(super) provider: &'a dyn LLMProvider,
    pub(super) request: LLMRequest,
    pub(super) runtime: &'a ProviderRequestRuntime,
    pub(super) cancellation: &'a SessionCancellation,
    pub(super) notice_target: Option<(&'a ZedAgent, &'a acp::SessionId)>,
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
            notice_target: None,
        }
    }

    pub(super) fn noticing(mut self, agent: &'a ZedAgent, session_id: &'a acp::SessionId) -> Self {
        self.notice_target = Some((agent, session_id));
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
async fn generate_once(
    provider: &dyn LLMProvider,
    request: &LLMRequest,
    runtime: &ProviderRequestRuntime,
    cancellation: &SessionCancellation,
) -> GenerateOutcome {
    let deadline_policy = runtime.deadline_policy();
    let total_deadline = deadline_after(deadline_policy.total_generation);
    tokio::select! {
        () = cancellation.cancelled() => GenerateOutcome::Cancelled,
        () = sleep_until_optional(total_deadline) => GenerateOutcome::Failed(provider_timeout_error(
            runtime.provider_name(),
            StreamTimeoutPhase::TotalGeneration,
            deadline_policy.total_generation,
        )),
        result = provider.generate(request.clone()).instrument(tracing::info_span!(
            "acp_provider_generate",
            provider_class = runtime.provider_class().as_str()
        )) => match result {
            Ok(response) => GenerateOutcome::Completed(Box::new(response)),
            Err(error) => GenerateOutcome::Failed(error),
        },
    }
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

struct RetryFailureNotice<'a> {
    notice_target: Option<(&'a ZedAgent, &'a acp::SessionId)>,
    runtime: &'a ProviderRequestRuntime,
    error: &'a LLMError,
    retry_delay: Option<Duration>,
    observed_at: SystemTime,
    metric_sink: &'a ProviderMetricSink,
}

async fn publish_retry_notice(notice: RetryFailureNotice<'_>) {
    let RetryFailureNotice {
        notice_target,
        runtime,
        error,
        retry_delay,
        observed_at,
        metric_sink,
    } = notice;
    let Some((agent, session_id)) = notice_target else {
        return;
    };
    agent
        .publish_rate_limit_notice_with_context(RateLimitNotice {
            session_id,
            provider: runtime.provider_name(),
            error,
            retry_delay,
            observed_at,
            metric_sink,
            provider_class: runtime.provider_class(),
        })
        .await;
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
    let RetryRequest {
        provider,
        request,
        runtime,
        cancellation,
        notice_target,
    } = retry_request;
    let policy = runtime.retry_policy();
    let mut attempt_index = 0;
    let mut backoff = RetryBackoff::new();

    loop {
        let permit = runtime.acquire(cancellation).await?;
        let mut telemetry = GenerationTelemetry::start(runtime, metric_sink.clone(), ProviderMode::Buffered);
        telemetry.record_attempt(runtime).await;
        match generate_once(provider, &request, runtime, cancellation).await {
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
                publish_retry_notice(RetryFailureNotice {
                    notice_target,
                    runtime,
                    error: &error,
                    retry_delay,
                    observed_at,
                    metric_sink,
                })
                .await;
                if !decision.retryable {
                    return Err(ProviderCallError::Failed(SafeProviderProjection::from_llm_error(&error)));
                }
                let delay = retry_delay.unwrap_or_else(|| policy.delay_for_attempt(AttemptIndex::new(attempt_index)));
                log_retry_scheduled(runtime, &policy, attempt_index, delay);
                cancellable_backoff(delay, cancellation).await?;
                attempt_index = attempt_index.saturating_add(1);
            }
        }
    }
}

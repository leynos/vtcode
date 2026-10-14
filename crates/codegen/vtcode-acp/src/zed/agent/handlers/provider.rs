use super::*;
use crate::zed::types::SessionCancellation;

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
    Failed(String),
}

impl From<ProviderAdmissionError> for ProviderCallError {
    fn from(error: ProviderAdmissionError) -> Self {
        match error {
            ProviderAdmissionError::Cancelled => Self::Cancelled,
            other => Self::Failed(other.to_string()),
        }
    }
}

pub(super) async fn cancellable_backoff(
    delay: Duration,
    cancellation: &SessionCancellation,
) -> Result<(), ProviderCallError> {
    tokio::select! {
        () = cancellation.cancelled() => Err(ProviderCallError::Cancelled),
        () = tokio::time::sleep(delay) => Ok(()),
    }
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
    pub(super) fn from_failure(content: &str, reasoning: &str, error: &str) -> Self {
        let sanitized_error = strip_ansi_codes(error).trim().to_string();
        let error_detail = if sanitized_error.is_empty() {
            "The provider did not report any additional details."
        } else {
            sanitized_error.as_str()
        };
        let notice = format!(
            "The provider could not complete this turn. You can retry the prompt.\n\nProvider error: {error_detail}"
        );
        let visible_update = if content.is_empty() {
            notice
        } else {
            format!("\n\n{notice}")
        };
        let message_content = format!("{content}{visible_update}");
        let message = incomplete_assistant_message(&message_content, reasoning, error_detail);
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
    message.metadata = Some(MessageMetadata::incomplete_llm_response(
        unix_timestamp_millis(),
        message.estimate_tokens(),
        strip_ansi_codes(error).trim(),
    ));
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
    // Starts before calling the provider router, so first-token and total
    // latency include any wait at the router's shared admission gate.
    started_at: Instant,
    pub(super) first_output_at: Option<Instant>,
    estimated_output_tokens: u64,
    runtime: ProviderRequestRuntime,
    finished: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ProviderErrorTelemetry<'a> {
    pub(super) code: Option<&'a str>,
    pub(super) status: Option<u16>,
    pub(super) timeout_phase: Option<&'static str>,
}

pub(super) fn provider_error_telemetry(error: &LLMError) -> ProviderErrorTelemetry<'_> {
    let LLMError::Network { metadata: Some(metadata), .. } = error else {
        return ProviderErrorTelemetry::default();
    };
    let code = metadata.code.as_deref();
    let timeout_phase = match code {
        Some("acp_timeout_first_token") => Some("first_token"),
        Some("acp_timeout_inter_token_idle") => Some("inter_token_idle"),
        Some("acp_timeout_total_generation") => Some("total_generation"),
        _ => None,
    };
    ProviderErrorTelemetry {
        code: if timeout_phase.is_some() { None } else { code },
        status: metadata.status,
        timeout_phase,
    }
}
impl GenerationTelemetry {
    pub(super) fn start(runtime: &ProviderRequestRuntime) -> Self {
        Self {
            started_at: Instant::now(),
            first_output_at: None,
            estimated_output_tokens: 0,
            runtime: runtime.clone(),
            finished: false,
        }
    }

    pub(super) fn observe_output(&mut self, runtime: &ProviderRequestRuntime, delta: &str, retry_count: u32) {
        let estimated_delta = u64::try_from(delta.chars().count().div_ceil(4)).unwrap_or(u64::MAX);
        self.estimated_output_tokens = self.estimated_output_tokens.saturating_add(estimated_delta);
        if self.first_output_at.is_some() {
            return;
        }

        let now = Instant::now();
        self.first_output_at = Some(now);
        let snapshot = runtime.telemetry_snapshot();
        info!(
            provider = runtime.provider_name(),
            time_to_first_token_ms = duration_millis(now.duration_since(self.started_at)),
            retry_count,
            acp_local_queue_depth = snapshot.acp_local_queue_depth,
            active_acp_requests = snapshot.active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            "ACP provider produced its first output"
        );
    }

    pub(super) fn complete(
        &mut self,
        runtime: &ProviderRequestRuntime,
        response: &LLMResponse,
        retry_count: u32,
        buffered: bool,
    ) {
        let elapsed = self.started_at.elapsed();
        let elapsed_ms = duration_millis(elapsed).max(1);
        let (output_tokens, token_count_source) = response
            .usage
            .as_ref()
            .filter(|usage| usage.completion_tokens > 0)
            .map(|usage| (u64::from(usage.completion_tokens), "provider"))
            .unwrap_or((self.estimated_output_tokens, "estimated"));
        let tokens_per_second = output_tokens.saturating_mul(1_000) / elapsed_ms;
        runtime.record_attempt_succeeded(
            elapsed_ms,
            self.first_output_at
                .map(|first| duration_millis(first.duration_since(self.started_at))),
        );
        self.finished = true;
        let snapshot = runtime.telemetry_snapshot();
        info!(
            provider = runtime.provider_name(),
            generation_elapsed_ms = elapsed_ms,
            time_to_first_token_ms = self
                .first_output_at
                .map(|first| duration_millis(first.duration_since(self.started_at)))
                .unwrap_or(elapsed_ms),
            ttft_observation = if buffered { "buffered_response" } else { "stream_event" },
            output_tokens,
            token_count_source,
            tokens_per_second,
            retry_count,
            acp_local_queue_depth = snapshot.acp_local_queue_depth,
            active_acp_requests = snapshot.active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            "ACP provider generation completed"
        );
    }

    pub(super) fn failed(
        &mut self,
        runtime: &ProviderRequestRuntime,
        retry_count: u32,
        retry_disposition: &'static str,
        error: &LLMError,
    ) {
        let elapsed_ms = duration_millis(self.started_at.elapsed());
        let first_output_ms = self
            .first_output_at
            .map(|first| duration_millis(first.duration_since(self.started_at)));
        runtime.record_attempt_failed(elapsed_ms, first_output_ms);
        if retry_disposition == "retry_scheduled" {
            runtime.record_retry_scheduled();
        }
        self.finished = true;
        let snapshot = runtime.telemetry_snapshot();
        let error_telemetry = provider_error_telemetry(error);
        warn!(
            provider = runtime.provider_name(),
            generation_elapsed_ms = duration_millis(self.started_at.elapsed()),
            retry_count,
            max_retries = runtime.retry_policy().max_attempts.saturating_sub(1),
            retry_disposition,
            acp_local_queue_depth = snapshot.acp_local_queue_depth,
            active_acp_requests = snapshot.active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            provider_error_code = ?error_telemetry.code,
            provider_error_status = ?error_telemetry.status,
            provider_timeout_phase = error_telemetry.timeout_phase,
            "ACP provider generation attempt failed"
        );
    }
}

impl Drop for GenerationTelemetry {
    fn drop(&mut self) {
        if !self.finished {
            let first_output_ms = self
                .first_output_at
                .map(|first| duration_millis(first.duration_since(self.started_at)));
            self.runtime
                .record_attempt_cancelled(duration_millis(self.started_at.elapsed()), first_output_ms);
            self.finished = true;
        }
    }
}

pub(super) fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(super) fn retry_disposition(decision: &RetryDecision) -> &'static str {
    if decision.retryable {
        "retry_scheduled"
    } else if decision.category.is_retryable() {
        "retry_exhausted"
    } else {
        "non_retryable"
    }
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

pub(super) async fn generate_with_retry(
    provider: &dyn LLMProvider,
    request: LLMRequest,
    runtime: &ProviderRequestRuntime,
    cancellation: &SessionCancellation,
) -> Result<LLMResponse, ProviderCallError> {
    let policy = runtime.retry_policy();
    let mut attempt_index = 0;

    loop {
        let permit = runtime.acquire(cancellation).await?;
        let mut telemetry = GenerationTelemetry::start(runtime);
        let deadline_policy = runtime.deadline_policy();
        let total_deadline = deadline_after(deadline_policy.total_generation);
        let result = tokio::select! {
            () = cancellation.cancelled() => return Err(ProviderCallError::Cancelled),
            () = sleep_until_optional(total_deadline) => {
                Err(provider_timeout_error(
                    runtime.provider_name(),
                    StreamTimeoutPhase::TotalGeneration,
                    deadline_policy.total_generation,
                ))
            }
            result = provider.generate(request.clone()).instrument(tracing::info_span!(
                "acp_provider_generate",
                provider = runtime.provider_name()
            )) => result,
        };
        match result {
            Ok(response) => {
                if let Some(content) = response.content.as_deref() {
                    telemetry.observe_output(runtime, content, attempt_index);
                }
                if let Some(reasoning) = response.reasoning.as_deref() {
                    telemetry.observe_output(runtime, reasoning, attempt_index);
                }
                telemetry.complete(runtime, &response, attempt_index, true);
                drop(permit);
                return Ok(response);
            }
            Err(error) => {
                let decision = policy.decision_for_llm_error(&error, attempt_index);
                telemetry.failed(runtime, attempt_index, retry_disposition(&decision), &error);
                drop(permit);
                if !decision.retryable {
                    return Err(ProviderCallError::Failed(error.to_string()));
                }
                let delay = decision.delay.unwrap_or_else(|| policy.delay_for_attempt(attempt_index));
                info!(
                    provider = runtime.provider_name(),
                    next_attempt = attempt_index + 2,
                    retry_count = attempt_index + 1,
                    max_retries = policy.max_attempts.saturating_sub(1),
                    ?delay,
                    "Retrying transient ACP provider request"
                );
                cancellable_backoff(delay, cancellation).await?;
                attempt_index = attempt_index.saturating_add(1);
            }
        }
    }
}

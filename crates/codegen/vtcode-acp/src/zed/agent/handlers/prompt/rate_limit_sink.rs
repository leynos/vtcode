//! Routing for provider attempt failures into ACP rate-limit notices.

use super::*;

/// Routing for one session's provider attempt failures into ACP rate-limit
/// notices.
///
/// This is the ACP-boundary implementation of [`AttemptFailureSink`]: retry
/// orchestration names only the port, so notice construction and delivery stay
/// with the rest of the prompt presentation logic.
pub(in crate::zed::agent::handlers) struct RateLimitNoticeSink<'a> {
    agent: &'a ZedAgent,
    session_id: &'a acp::SessionId,
}

impl<'a> RateLimitNoticeSink<'a> {
    /// Bind the sink to one session's ACP client surface.
    pub(in crate::zed::agent::handlers) fn new(agent: &'a ZedAgent, session_id: &'a acp::SessionId) -> Self {
        Self { agent, session_id }
    }
}

impl AttemptFailureSink for RateLimitNoticeSink<'_> {
    /// Publish the failed attempt as a rate-limit notice for this session.
    fn announce<'a>(&'a self, failure: AttemptFailure<'a>) -> AttemptFailureFuture<'a> {
        Box::pin(async move {
            self.agent
                .publish_rate_limit_notice_with_context(RateLimitNotice {
                    session_id: self.session_id,
                    provider: failure.provider,
                    error: failure.error,
                    retry_delay: failure.retry_delay,
                    observed_at: failure.observed_at,
                    metric_sink: failure.metric_sink,
                    provider_class: failure.provider_class,
                })
                .await;
        })
    }
}

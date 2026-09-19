//! Domain-specific retry adapters for VT Code.
//!
//! The retry *policy math* (attempt budgets, backoff, jitter, category-based
//! decisions) lives in [`vtcode_commons::retry`]; this module re-exports the
//! canonical [`RetryPolicy`] / [`RetryDecision`] and layers the workspace's
//! domain knowledge on top via [`RetryPolicyCoreExt`]: typed error downcasts
//! (`VtCodeError`, `LLMError`, tool errors), LLM `Retry-After` extraction,
//! and the tool-aware command-timeout rule.

use std::future::Future;
use std::result::Result as StdResult;
use std::time::{Duration, SystemTime};

use crate::error::{ErrorCategory, VtCodeError};
use crate::retry_after::{retry_after_from_llm_metadata_at, retry_after_is_http_date};
use crate::tools::registry::ToolExecutionError;
use crate::tools::tool_intent::is_command_tool;
use crate::tools::unified_error::UnifiedToolError;
use vtcode_commons::llm::{LLMError, LLMErrorMetadata};

pub use vtcode_commons::retry::{RetryBackoff, RetryDecision, RetryPolicy};

/// Domain-aware retry decisions layered over the shared [`RetryPolicy`].
///
/// Import this trait to call the `decision_for_*` / `step_for_*` methods on
/// a policy; the policy struct itself is defined in `vtcode-commons`.
pub trait RetryPolicyCoreExt {
    fn decision_for_vtcode_error(
        &self,
        error: &VtCodeError,
        attempt_index: u32,
        tool_name: Option<&str>,
    ) -> RetryDecision;

    fn decision_for_anyhow(&self, error: &anyhow::Error, attempt_index: u32, tool_name: Option<&str>) -> RetryDecision;

    /// Classify an LLM error using the compatibility wall-clock observation.
    fn decision_for_llm_error(&self, error: &LLMError, attempt_index: u32) -> RetryDecision;

    /// Classify an LLM error against the time at which it was observed.
    ///
    /// Existing implementers inherit a compatibility fallback that delegates
    /// to [`Self::decision_for_llm_error`] and ignores `observed_at`.
    /// [`RetryPolicy`] overrides this method to honour the supplied time.
    fn decision_for_llm_error_at(
        &self,
        error: &LLMError,
        attempt_index: u32,
        observed_at: SystemTime,
    ) -> RetryDecision {
        let _ = observed_at;
        self.decision_for_llm_error(error, attempt_index)
    }

    /// Classify an LLM error while retaining a provider retry floor across
    /// attempts in the current logical segment, using the compatibility
    /// wall-clock observation.
    fn decision_for_llm_error_with_backoff(
        &self,
        error: &LLMError,
        attempt_index: u32,
        backoff: &mut RetryBackoff,
    ) -> RetryDecision;

    /// Classify an LLM error with a retry floor against its observation time.
    ///
    /// Existing implementers inherit a compatibility fallback that delegates
    /// to [`Self::decision_for_llm_error_with_backoff`] and ignores
    /// `observed_at`. [`RetryPolicy`] overrides this method to honour the
    /// supplied time.
    fn decision_for_llm_error_with_backoff_at(
        &self,
        error: &LLMError,
        attempt_index: u32,
        backoff: &mut RetryBackoff,
        observed_at: SystemTime,
    ) -> RetryDecision {
        let _ = observed_at;
        self.decision_for_llm_error_with_backoff(error, attempt_index, backoff)
    }

    fn decision_for_tool_error(&self, error: &UnifiedToolError, attempt_index: u32) -> RetryDecision;

    fn decision_for_tool_execution_error(&self, error: &ToolExecutionError, attempt_index: u32) -> RetryDecision;

    /// Classify a `VtCodeError` failure into a typed [`RetryStep`].
    ///
    /// This consolidates the "decision -> sleep or give-up" branching used by
    /// agent-level retry loops, removing the brittle
    /// `decision.delay.expect("retryable decisions need delay")` calls and
    /// guaranteeing a delay is always available for retryable steps.
    fn step_for_vtcode_error(&self, error: VtCodeError, attempt_index: u32, tool_name: Option<&str>) -> RetryStep;

    fn apply_to_tool_execution_error(
        &self,
        error: ToolExecutionError,
        attempt_index: u32,
        tool_name: Option<&str>,
    ) -> ToolExecutionError;
}

impl RetryPolicyCoreExt for RetryPolicy {
    fn decision_for_vtcode_error(
        &self,
        error: &VtCodeError,
        attempt_index: u32,
        tool_name: Option<&str>,
    ) -> RetryDecision {
        decision_for_category_with_tool(self, error.category, attempt_index, error.retry_after(), tool_name)
    }

    fn decision_for_anyhow(&self, error: &anyhow::Error, attempt_index: u32, tool_name: Option<&str>) -> RetryDecision {
        if let Some(vtcode_error) = error.downcast_ref::<VtCodeError>() {
            return self.decision_for_vtcode_error(vtcode_error, attempt_index, tool_name);
        }
        if let Some(llm_error) = error.downcast_ref::<LLMError>() {
            return self.decision_for_llm_error(llm_error, attempt_index);
        }
        if let Some(tool_error) = error.downcast_ref::<UnifiedToolError>() {
            let tool_name = tool_name.or_else(|| {
                tool_error
                    .debug_context
                    .as_ref()
                    .map(|ctx| ctx.tool_name.as_str())
                    .filter(|tool_name| !tool_name.is_empty())
            });
            return decision_for_category_with_tool(self, tool_error.category(), attempt_index, None, tool_name);
        }

        let category = vtcode_commons::classify_anyhow_error(error);
        decision_for_category_with_tool(self, category, attempt_index, None, tool_name)
    }

    fn decision_for_llm_error(&self, error: &LLMError, attempt_index: u32) -> RetryDecision {
        self.decision_for_llm_error_at(error, attempt_index, SystemTime::now())
    }

    fn decision_for_llm_error_at(
        &self,
        error: &LLMError,
        attempt_index: u32,
        observed_at: SystemTime,
    ) -> RetryDecision {
        let retry_after =
            llm_metadata(error).and_then(|metadata| retry_after_from_llm_metadata_at(metadata, observed_at));
        decision_for_category_with_tool(self, ErrorCategory::from(error), attempt_index, retry_after, None)
    }

    fn decision_for_llm_error_with_backoff(
        &self,
        error: &LLMError,
        attempt_index: u32,
        backoff: &mut RetryBackoff,
    ) -> RetryDecision {
        self.decision_for_llm_error_with_backoff_at(error, attempt_index, backoff, SystemTime::now())
    }

    fn decision_for_llm_error_with_backoff_at(
        &self,
        error: &LLMError,
        attempt_index: u32,
        backoff: &mut RetryBackoff,
        observed_at: SystemTime,
    ) -> RetryDecision {
        decision_for_llm_error_with_backoff_at_impl(self, error, attempt_index, backoff, observed_at)
    }

    fn decision_for_tool_error(&self, error: &UnifiedToolError, attempt_index: u32) -> RetryDecision {
        let tool_name = error
            .debug_context
            .as_ref()
            .map(|ctx| ctx.tool_name.as_str())
            .filter(|tool_name| !tool_name.is_empty());
        decision_for_category_with_tool(self, error.category(), attempt_index, None, tool_name)
    }

    fn decision_for_tool_execution_error(&self, error: &ToolExecutionError, attempt_index: u32) -> RetryDecision {
        decision_for_category_with_tool(
            self,
            error.category,
            attempt_index,
            error.retry_after(),
            Some(error.tool_name.as_str()),
        )
    }

    fn step_for_vtcode_error(&self, error: VtCodeError, attempt_index: u32, tool_name: Option<&str>) -> RetryStep {
        let decision = self.decision_for_vtcode_error(&error, attempt_index, tool_name);
        if decision.retryable {
            let delay = decision.delay.unwrap_or_else(|| self.delay_for_attempt(attempt_index));
            RetryStep::Backoff { delay, decision, error }
        } else {
            RetryStep::GiveUp { decision, error }
        }
    }

    fn apply_to_tool_execution_error(
        &self,
        error: ToolExecutionError,
        attempt_index: u32,
        tool_name: Option<&str>,
    ) -> ToolExecutionError {
        let decision = decision_for_category_with_tool(
            self,
            error.category,
            attempt_index,
            error.retry_after(),
            tool_name.or(Some(error.tool_name.as_str())),
        );
        error.with_retry_decision(decision)
    }
}

fn decision_for_llm_error_with_backoff_at_impl(
    policy: &RetryPolicy,
    error: &LLMError,
    attempt_index: u32,
    backoff: &mut RetryBackoff,
    now: SystemTime,
) -> RetryDecision {
    let Some(metadata) = llm_metadata(error) else {
        return policy.decision_for_category_with_backoff(ErrorCategory::from(error), attempt_index, None, backoff);
    };
    let retry_after = retry_after_from_llm_metadata_at(metadata, now);
    if !retry_after_is_http_date(metadata) {
        return policy.decision_for_category_with_backoff(
            ErrorCategory::from(error),
            attempt_index,
            retry_after,
            backoff,
        );
    }

    let reset_floor = metadata
        .rate_limit
        .as_ref()
        .and_then(|rate_limit| rate_limit.reset_after_millis)
        .map(Duration::from_millis);
    let mut decision =
        policy.decision_for_category_with_backoff(ErrorCategory::from(error), attempt_index, reset_floor, backoff);
    if let Some(retry_after) = retry_after
        && decision.retryable
    {
        let local_delay = decision.delay.unwrap_or(Duration::ZERO);
        decision.delay = Some(local_delay.max(retry_after));
        decision.retry_after = Some(retry_after);
    }
    decision
}

fn decision_for_category_with_tool(
    policy: &RetryPolicy,
    category: ErrorCategory,
    attempt_index: u32,
    retry_after: Option<Duration>,
    tool_name: Option<&str>,
) -> RetryDecision {
    if is_non_retryable_command_timeout(category, tool_name) {
        return RetryDecision {
            category,
            retryable: false,
            delay: None,
            retry_after,
        };
    }

    policy.decision_for_category(category, attempt_index, retry_after)
}

/// The single tool-aware retry rule: command tool timeouts are never
/// retryable because the underlying process may still be running, so a
/// retry can contend for locks or duplicate side effects.
pub(crate) fn is_non_retryable_command_timeout(category: ErrorCategory, tool_name: Option<&str>) -> bool {
    matches!(category, ErrorCategory::Timeout) && tool_name.is_some_and(is_command_tool)
}

/// Typed step produced by [`RetryPolicyCoreExt::step_for_vtcode_error`].
///
/// Callers match on this instead of re-deriving the
/// `if decision.retryable { sleep(decision.delay.expect(...)) }` pattern.
#[derive(Debug)]
pub enum RetryStep {
    /// Wait `delay` then retry; `error` is the failure being retried.
    Backoff {
        delay: Duration,
        decision: RetryDecision,
        error: VtCodeError,
    },
    /// Give up immediately and surface `error`.
    GiveUp {
        decision: RetryDecision,
        error: VtCodeError,
    },
}

/// Lifecycle event emitted by [`run_with_retry`] so callers can attach
/// logging, metrics, or stats updates without re-implementing the loop.
///
/// `category_was_retryable` is provided alongside `GiveUp` and `Backoff`
/// because call sites frequently need to distinguish "non-retryable
/// category" from "retryable category, budget exhausted" — the distinction
/// is otherwise invisible once `step_for_vtcode_error` collapses both
/// into `GiveUp`.
#[derive(Debug)]
pub enum RetryEvent<'a> {
    /// An attempt is about to start. `attempt` is 0-indexed.
    AttemptStart { attempt: u32, max_attempts: u32 },
    /// The operation succeeded on the given attempt.
    Success { attempt: u32 },
    /// The policy decided to give up and surface `error` immediately.
    GiveUp {
        attempt: u32,
        error: &'a VtCodeError,
        decision: &'a RetryDecision,
        category_was_retryable: bool,
    },
    /// The policy decided to back off and retry. The driver will sleep
    /// for `delay` before invoking the operation again.
    Backoff {
        attempt: u32,
        error: &'a VtCodeError,
        decision: &'a RetryDecision,
        delay: Duration,
        category_was_retryable: bool,
    },
    /// All attempts were exhausted. `last_error` is the most recent
    /// error captured from a `Backoff` step.
    Exhausted { last_error: Option<&'a VtCodeError> },
}

/// Drive a retry loop per `policy`, invoking `on_event` for each
/// lifecycle event. Returns the first successful result, or the final
/// error if the policy gives up or all attempts are exhausted.
///
/// `state` is reborrowed mutably and passed to each callback on every
/// invocation, so callers can use it to thread a `&mut self` (or any
/// other mutable context) through the loop without resorting to
/// `RefCell` or split borrows. The `operation` callback must return a
/// boxed future so the helper can `await` it without tying its lifetime
/// to the closure's own borrow of `state`.
///
/// The returned future is `Send` so it can be passed to `tokio::spawn`.
///
/// `synthesize_exhausted_error` is invoked only in the (degenerate)
/// case where the loop completes without ever recording a `Backoff`
/// error. The closure receives the `&RetryPolicy` so it can attach
/// site-specific context (e.g. `policy.max_attempts`) without having
/// to capture the policy in its environment — which is exactly the
/// pattern that conflicts with `&mut state`.
#[allow(
    clippy::too_many_arguments,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
pub async fn run_with_retry<T, E, S, F, OnEvent, Synthesize>(
    policy: &RetryPolicy,
    state: &mut S,
    mut on_event: OnEvent,
    mut operation: F,
    synthesize_exhausted_error: Synthesize,
) -> crate::error::Result<T>
where
    F: for<'a> FnMut(&'a mut S) -> std::pin::Pin<Box<dyn Future<Output = StdResult<T, E>> + Send + 'a>>,
    E: Into<VtCodeError>,
    OnEvent: FnMut(&mut S, RetryEvent<'_>),
    Synthesize: FnOnce(&RetryPolicy) -> VtCodeError,
{
    use tokio::time::sleep;

    let mut last_error: Option<VtCodeError> = None;
    for attempt in 0..policy.max_attempts {
        on_event(state, RetryEvent::AttemptStart { attempt, max_attempts: policy.max_attempts });
        match operation(state).await {
            Ok(value) => {
                on_event(state, RetryEvent::Success { attempt });
                return Ok(value);
            }
            Err(err) => {
                let err: VtCodeError = err.into();
                let category_was_retryable = err.category.is_retryable();
                let step = policy.step_for_vtcode_error(err, attempt, None);
                match step {
                    RetryStep::GiveUp { decision, error } => {
                        on_event(
                            state,
                            RetryEvent::GiveUp {
                                attempt,
                                error: &error,
                                decision: &decision,
                                category_was_retryable,
                            },
                        );
                        return Err(error);
                    }
                    RetryStep::Backoff { delay, decision, error } => {
                        on_event(
                            state,
                            RetryEvent::Backoff {
                                attempt,
                                error: &error,
                                decision: &decision,
                                delay,
                                category_was_retryable,
                            },
                        );
                        last_error = Some(error);
                        sleep(delay).await;
                    }
                }
            }
        }
    }
    let final_error = last_error.unwrap_or_else(|| synthesize_exhausted_error(policy));
    on_event(state, RetryEvent::Exhausted { last_error: Some(&final_error) });
    Err(final_error)
}

fn llm_metadata(error: &LLMError) -> Option<&LLMErrorMetadata> {
    match error {
        LLMError::Authentication { metadata, .. }
        | LLMError::RateLimit { metadata }
        | LLMError::InvalidRequest { metadata, .. }
        | LLMError::Network { metadata, .. }
        | LLMError::Provider { metadata, .. } => metadata.as_deref(),
    }
}

pub fn decision_for_vtcode_error(
    error: &VtCodeError,
    attempt_index: u32,
    tool_name: Option<&str>,
    policy_override: Option<&RetryPolicy>,
) -> RetryDecision {
    let policy = policy_override.unwrap_or(&RetryPolicy::DEFAULT);
    policy.decision_for_vtcode_error(error, attempt_index, tool_name)
}

pub fn decision_for_anyhow_error(
    error: &anyhow::Error,
    attempt_index: u32,
    tool_name: Option<&str>,
    policy_override: Option<&RetryPolicy>,
) -> RetryDecision {
    let policy = policy_override.unwrap_or(&RetryPolicy::DEFAULT);
    policy.decision_for_anyhow(error, attempt_index, tool_name)
}

#[cfg(test)]
mod tests;

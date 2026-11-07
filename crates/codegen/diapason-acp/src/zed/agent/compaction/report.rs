//! Terminal reporting for one automatic compaction attempt.
//!
//! An attempt yields a value describing what it achieved, and the bookkeeping
//! around it emits outcome counters, span fields, log records, and the closing
//! ACP activity update. Keeping that reporting beside the attempt result keeps
//! the compaction pipeline itself a linear sequence of explicit steps.

use super::super::ZedAgent;
use super::super::redaction::{elapsed_millis, identifier_correlation, log_compaction_update_failure};
use super::failure::{CompactionFailureKind, FailureDetail, classify, detail_of};
use crate::metrics::CompactionOutcome;
use anyhow::Result;
use std::time::Instant;
use tracing::{Instrument, field, info, info_span, warn};

/// What one automatic compaction attempt achieved.
pub(super) struct CompactionAttemptResult {
    /// Whether the history was replaced.
    pub(super) compacted: bool,
    /// Prompt tokens estimated from the adopted history, when it was replaced.
    pub(super) used_tokens_after: Option<usize>,
    /// The failure that ended the attempt, if any.
    pub(super) failure: Option<anyhow::Error>,
}

impl CompactionAttemptResult {
    /// Report an attempt that produced no compaction outcome.
    pub(super) const fn no_change() -> Self {
        Self {
            compacted: false,
            used_tokens_after: None,
            failure: None,
        }
    }

    /// Report an attempt that replaced the history and fits the budget.
    pub(super) const fn compacted(used_tokens_after: usize) -> Self {
        Self {
            compacted: true,
            used_tokens_after: Some(used_tokens_after),
            failure: None,
        }
    }

    /// Report an attempt that replaced the history but still exceeds the
    /// provider admission budget.
    pub(super) fn compacted_over_budget(used_tokens_after: usize, error: anyhow::Error) -> Self {
        Self {
            compacted: true,
            used_tokens_after: Some(used_tokens_after),
            failure: Some(error),
        }
    }

    /// Report an attempt that ended in `error` without replacing the history.
    pub(super) fn failed(error: anyhow::Error) -> Self {
        Self {
            compacted: false,
            used_tokens_after: None,
            failure: Some(error),
        }
    }
}

/// A compacted history that the session has already adopted.
pub(super) struct AdoptedCompaction {
    /// Prompt tokens estimated from the adopted history.
    pub(super) prompt_tokens: usize,
    /// Whether the adopted history still exceeds the admission budget.
    pub(super) over_budget: bool,
}

/// The values the terminal compaction bookkeeping reports on.
pub(super) struct CompactionReport<'a> {
    pub(super) provider: &'a str,
    pub(super) model: &'a str,
    pub(super) prompt_tokens: usize,
    pub(super) session_key: &'a str,
    pub(super) elapsed_ms: u64,
}

/// Record a suppressed compaction failure at debug.
///
/// A typed failure's `Display` is only the fixed client-safe message, so
/// deriving the digest from the error here would give this record and the
/// operator warning two different correlations for the same failure. Both
/// callers instead share the one bounded [`FailureDetail`], and a failure with
/// no retained detail records none rather than inventing a digest.
fn log_suppressed_compaction_failure(detail: Option<&FailureDetail>, category: &'static str) {
    // Both records format the digest through `Display`, so an operator sees the
    // identical text in the debug record and the warning rather than one value
    // quoted and the other bare.
    match detail {
        Some(detail) => tracing::debug!(
            operation = "context_compaction",
            error_category = category,
            error_bytes = detail.bytes,
            detail_correlation = %detail.correlation,
            "Suppressed ACP failure"
        ),
        None => tracing::debug!(operation = "context_compaction", error_category = category, "Suppressed ACP failure"),
    }
}

impl ZedAgent {
    /// Record the terminal outcome of an attempt and return its category.
    ///
    /// Owns the outcome counters, the span fields and the closing log so the
    /// attempt pipeline stays a linear sequence of explicit steps.
    pub(super) fn record_compaction_outcome(
        &self,
        span: &tracing::Span,
        attempt: &CompactionAttemptResult,
        report: CompactionReport<'_>,
    ) -> Option<&'static str> {
        // Clients only ever see this closed-set category. The originating
        // error is untrusted, so its text is never logged and only a bounded
        // correlation digest of it is recorded.
        let failure_kind = attempt.failure.as_ref().map(classify);
        let failure_category = failure_kind.map(CompactionFailureKind::category);
        let outcome = CompactionOutcome::for_result(failure_kind.is_some(), attempt.compacted);
        self.compaction_recorder.record_finished(
            outcome,
            failure_kind == Some(CompactionFailureKind::Cancelled),
            report.elapsed_ms,
        );
        let _ = span.record("outcome", outcome.as_str());
        let _ = span.record("elapsed_ms", report.elapsed_ms);
        if let (Some(error), Some(category)) = (attempt.failure.as_ref(), failure_category) {
            let _ = span.record("error_category", category);
            let detail = detail_of(error);
            log_suppressed_compaction_failure(detail, category);
            Self::warn_compaction_failure(detail, &report, outcome.as_str(), category);
        } else {
            info!(
                provider_correlation = %identifier_correlation(report.provider),
                provider_bytes = report.provider.len(),
                model_correlation = %identifier_correlation(report.model),
                model_bytes = report.model.len(),
                prompt_tokens = report.prompt_tokens,
                compacted = attempt.compacted,
                elapsed_ms = report.elapsed_ms,
                session_correlation = %report.session_key,
                outcome = outcome.as_str(),
                "ACP automatic context compaction finished"
            );
        }
        failure_category
    }

    /// Warn about a failed attempt, adding the bounded source detail when known.
    ///
    /// A failure raised as a bare category has no source error, so its record
    /// carries the closed-set fields alone rather than an invented digest.
    fn warn_compaction_failure(
        detail: Option<&FailureDetail>,
        report: &CompactionReport<'_>,
        outcome: &str,
        category: &'static str,
    ) {
        match detail {
            Some(detail) => warn!(
                provider_correlation = %identifier_correlation(report.provider),
                provider_bytes = report.provider.len(),
                model_correlation = %identifier_correlation(report.model),
                model_bytes = report.model.len(),
                prompt_tokens = report.prompt_tokens,
                elapsed_ms = report.elapsed_ms,
                session_correlation = %report.session_key,
                outcome,
                error_category = category,
                error_bytes = detail.bytes,
                detail_correlation = %detail.correlation,
                "ACP automatic context compaction failed"
            ),
            None => warn!(
                provider_correlation = %identifier_correlation(report.provider),
                provider_bytes = report.provider.len(),
                model_correlation = %identifier_correlation(report.model),
                model_bytes = report.model.len(),
                prompt_tokens = report.prompt_tokens,
                elapsed_ms = report.elapsed_ms,
                session_correlation = %report.session_key,
                outcome,
                error_category = category,
                "ACP automatic context compaction failed"
            ),
        }
    }

    /// Publish one compaction activity update, logging fixed fields on failure.
    ///
    /// A serialization failure and a transport failure keep distinct stage
    /// labels so an operator can tell which half of the publish failed. The
    /// send is observed by a span and by the publication counters, so a
    /// client that stops accepting activity updates is measurable rather than
    /// merely logged.
    pub(super) async fn publish_compaction_update(
        &self,
        session_id: &crate::acp::SessionId,
        update: Result<crate::acp::SessionUpdate>,
        session_key: &str,
        publish_stage: &'static str,
        serialize_stage: &'static str,
    ) {
        let update = match update {
            Ok(update) => update,
            // Serialization never reaches the transport, so it is not a send
            // and moves neither the span's outcome nor the counters.
            Err(error) => {
                log_compaction_update_failure(&error, session_key, serialize_stage);
                return;
            }
        };
        let span = info_span!(
            "acp_compaction_update_publish",
            operation = "context_compaction_update",
            stage = publish_stage,
            session_correlation = %session_key,
            outcome = field::Empty,
            elapsed_ms = field::Empty,
        );
        let started_at = Instant::now();
        async {
            let result = self.send_update(session_id, update).await;
            let span = tracing::Span::current();
            let sent = result.is_ok();
            let _outcome_recorded = span.record("outcome", if sent { "sent" } else { "failed" });
            let _elapsed_ms_recorded = span.record("elapsed_ms", elapsed_millis(started_at));
            self.compaction_update_recorder.record(sent);
            if let Err(error) = &result {
                log_compaction_update_failure(error, session_key, publish_stage);
            }
        }
        .instrument(span)
        .await;
    }
}

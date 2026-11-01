//! Closed-set classification of automatic ACP compaction failures.
//!
//! Compaction failures reach two audiences. ACP clients receive a
//! `failureReason` field, and operators read warning events. Neither may see
//! provider detail, workspace paths, or archive contents, so both get a
//! category from a closed set. The originating error is retained for the
//! redacted debug helper in [`super::super::log_suppressed_error`].

use anyhow::Error;
use std::borrow::Cow;
use std::fmt;

/// Why an automatic ACP compaction attempt did not finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CompactionFailureKind {
    /// The owning session was cancelled while compacting.
    Cancelled,
    /// The provider total-generation deadline expired.
    Timeout,
    /// The prompt still exceeded the safe admission budget afterwards.
    AdmissionBudget,
    /// A `PreCompact` lifecycle hook failed.
    PreCompactHook,
    /// The provider request or any other unattributed stage failed.
    Provider,
}

impl CompactionFailureKind {
    /// Stable reason code shared by logs and client-facing payloads.
    pub(super) const fn category(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::AdmissionBudget => "admission_budget",
            Self::PreCompactHook => "pre_compact_hook",
            Self::Provider => "provider",
        }
    }
}

/// A typed compaction failure that survives `.context()` wrapping.
///
/// `anyhow` keeps context as a wrapper around the original error rather than
/// replacing it, so `downcast_ref` still finds this type after a caller adds
/// `.context(...)`. Classification therefore does not depend on the exact
/// wrapping depth of the returned error.
#[derive(Debug)]
pub(super) struct CompactionFailure {
    kind: CompactionFailureKind,
    /// Explanation safe to return to callers: built only from values this
    /// module controls, never from a provider or filesystem error.
    message: Cow<'static, str>,
    /// Untrusted detail, emitted only through the redacted debug helper.
    detail: Option<String>,
}

impl CompactionFailure {
    /// Assemble a failure from its category, client-safe message and detail.
    fn new(kind: CompactionFailureKind, message: impl Into<Cow<'static, str>>, detail: Option<String>) -> Self {
        Self { kind, message: message.into(), detail }
    }

    /// Create a failure for a session cancelled during compaction.
    pub(super) fn cancelled() -> Self {
        Self::new(CompactionFailureKind::Cancelled, "ACP compaction cancelled", None)
    }

    /// Create a failure for an expired provider total-generation deadline.
    pub(super) fn timeout() -> Self {
        Self::new(
            CompactionFailureKind::Timeout,
            "ACP compaction exceeded the provider total-generation timeout",
            None,
        )
    }

    /// Create a failure for a prompt that still exceeds the admission budget.
    pub(super) fn admission_budget(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(CompactionFailureKind::AdmissionBudget, message, None)
    }

    /// Create a failure for a failed `PreCompact` lifecycle hook.
    pub(super) fn pre_compact_hook(error: &Error) -> Self {
        Self::new(CompactionFailureKind::PreCompactHook, "ACP PreCompact hooks failed", Some(format!("{error:#}")))
    }

    /// Create a failure for an unattributed provider or pipeline error.
    pub(super) fn provider(error: &Error) -> Self {
        Self::new(
            CompactionFailureKind::Provider,
            "ACP automatic context compaction failed",
            Some(format!("{error:#}")),
        )
    }

    /// Create a provider-category failure from caller-supplied untrusted detail.
    pub(super) fn provider_detail(detail: impl Into<String>) -> Self {
        Self::new(CompactionFailureKind::Provider, "ACP automatic context compaction failed", Some(detail.into()))
    }

    /// Return this failure's category without consuming it.
    pub(super) const fn kind(&self) -> CompactionFailureKind {
        self.kind
    }

    /// Wrap this failure into the `anyhow` error the compaction pipeline uses.
    pub(super) fn into_error(self) -> Error {
        Error::new(self)
    }
}

impl fmt::Display for CompactionFailure {
    /// Write the client-safe message, never the untrusted detail.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CompactionFailure {}

/// Classify a compaction pipeline error into its closed-set category.
///
/// Errors raised outside this module (a poisoned lock, an unreadable session
/// archive) fall back to [`CompactionFailureKind::Provider`]; their detail is
/// still recorded by the redacted debug helper.
pub(super) fn classify(error: &Error) -> CompactionFailureKind {
    error
        .downcast_ref::<CompactionFailure>()
        .map_or(CompactionFailureKind::Provider, CompactionFailure::kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert each typed failure classifies as its own category.
    #[test]
    fn typed_failures_are_classified_by_their_own_category() {
        let cases = [
            (CompactionFailure::cancelled(), CompactionFailureKind::Cancelled),
            (CompactionFailure::timeout(), CompactionFailureKind::Timeout),
            (
                CompactionFailure::admission_budget("prompt still over budget"),
                CompactionFailureKind::AdmissionBudget,
            ),
            (CompactionFailure::provider_detail("provider exploded"), CompactionFailureKind::Provider),
        ];

        for (failure, expected) in cases {
            assert_eq!(classify(&failure.into_error()), expected);
        }
    }

    /// Assert `.context()` wrapping does not hide the failure category.
    #[test]
    fn context_wrapping_preserves_the_failure_category() {
        let error = CompactionFailure::cancelled()
            .into_error()
            .context("ACP automatic context compaction failed");

        assert_eq!(classify(&error), CompactionFailureKind::Cancelled);
    }

    /// Assert untyped errors classify as provider failures.
    #[test]
    fn untyped_errors_fall_back_to_the_provider_category() {
        let error = anyhow::anyhow!("session archive could not be read");

        assert_eq!(classify(&error), CompactionFailureKind::Provider);
    }

    /// Assert client-facing messages exclude untrusted provider detail.
    #[test]
    fn client_facing_messages_never_include_untrusted_detail() {
        let failure = CompactionFailure::provider_detail("/secret/workspace/archive.json failed");
        let message = failure.to_string();

        assert_eq!(message, "ACP automatic context compaction failed");
        assert!(!message.contains("/secret"), "detail must stay out of the client message");
        assert_eq!(failure.kind().category(), "provider");
    }
}

//! Closed-set classification of automatic ACP compaction failures.
//!
//! Compaction failures reach two audiences. ACP clients receive a
//! `failureReason` field, and operators read warning events. Neither may see
//! provider detail, workspace paths, archive contents, hook text, or personal
//! data, so both get a category from a closed set. The originating error is
//! deliberately not retained: it may embed untrusted text, and nothing in this
//! type may expose it to a log or a client.

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
}

impl CompactionFailure {
    /// Assemble a failure from its category and client-safe message.
    const fn new(kind: CompactionFailureKind, message: Cow<'static, str>) -> Self {
        Self { kind, message }
    }

    /// Create a failure for a session cancelled during compaction.
    pub(super) const fn cancelled() -> Self {
        Self::new(CompactionFailureKind::Cancelled, Cow::Borrowed("ACP compaction cancelled"))
    }

    /// Create a failure for an expired provider total-generation deadline.
    pub(super) const fn timeout() -> Self {
        Self::new(
            CompactionFailureKind::Timeout,
            Cow::Borrowed("ACP compaction exceeded the provider total-generation timeout"),
        )
    }

    /// Create a failure for a prompt that still exceeds the admission budget.
    pub(super) fn admission_budget(message: impl Into<Cow<'static, str>>) -> Self {
        Self::new(CompactionFailureKind::AdmissionBudget, message.into())
    }

    /// Create a failure for a failed `PreCompact` lifecycle hook.
    ///
    /// `_error` is accepted so call sites read the same as the provider
    /// boundary, but it is deliberately not retained: hook output may embed
    /// script paths, environment values, or workspace content.
    pub(super) const fn pre_compact_hook(_error: &Error) -> Self {
        Self::new(CompactionFailureKind::PreCompactHook, Cow::Borrowed("ACP PreCompact hooks failed"))
    }

    /// Create a failure for an unattributed provider or pipeline error.
    ///
    /// `_error` is deliberately not retained: provider payloads, transport
    /// URLs, and model output may contain personal data or secrets.
    pub(super) const fn provider(_error: &Error) -> Self {
        Self::new(CompactionFailureKind::Provider, Cow::Borrowed("ACP automatic context compaction failed"))
    }

    /// Create a failure for provider admission that closed or timed out.
    pub(super) const fn provider_admission() -> Self {
        Self::new(
            CompactionFailureKind::Provider,
            Cow::Borrowed("ACP compaction could not start: provider admission unavailable"),
        )
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
/// archive) fall back to [`CompactionFailureKind::Provider`]; their text is
/// reduced to that category before it reaches a log or a client.
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
            (CompactionFailure::provider_admission(), CompactionFailureKind::Provider),
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

    /// Assert neither the display nor the error chain carries provider detail.
    #[test]
    fn provider_detail_never_reaches_the_display_or_the_error_chain() {
        let source = anyhow::anyhow!(
            "provider payload POST https://api.example.invalid/v1/chat failed for \
             /secret/workspace/archive.json with alice@example.com"
        );
        let failure = CompactionFailure::provider(&source);
        let client_message = failure.to_string();
        let category = failure.kind().category();
        let operator_chain = format!("{:#}", failure.into_error());

        assert_eq!(client_message, "ACP automatic context compaction failed");
        assert_eq!(operator_chain, "ACP automatic context compaction failed");
        assert_eq!(category, "provider");
        for leaked in [
            "api.example.invalid",
            "/secret/workspace",
            "alice@example.com",
            "provider payload",
        ] {
            assert!(!operator_chain.contains(leaked), "{leaked} must stay out of the chain, got {operator_chain}");
        }
    }

    /// Assert hook detail is reduced to the closed-set category.
    #[test]
    fn hook_detail_never_reaches_the_display_or_the_error_chain() {
        let source = anyhow::anyhow!("hook script /home/private/hooks/pre.sh exited 3");
        let failure = CompactionFailure::pre_compact_hook(&source);
        let client_message = failure.to_string();
        let operator_chain = format!("{:#}", failure.into_error());

        assert_eq!(client_message, "ACP PreCompact hooks failed");
        assert_eq!(operator_chain, "ACP PreCompact hooks failed");
        assert!(!operator_chain.contains("pre.sh"), "hook text must stay out, got {operator_chain}");
    }
}

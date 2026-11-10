//! Certification of ACP read-file versions against the on-disk bytes.
//!
//! The ACP client serves the editor buffer, which may be unsaved, so a read is
//! certified only when the returned text matches the bytes on disk. A windowed
//! read must equal the same line window projected from the disk; a whole-file
//! read must hash to those bytes. Any mismatch, non-UTF-8 content, or failed
//! read withholds the version rather than certifying bytes the model never saw;
//! `apply_patch` treats an absent precondition as omitted.

use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use tracing::warn;

use super::super::types::SessionHandle;
use super::ZedAgent;
use diapason_core::metrics::{MetricsCollector, PatchCertificationOutcome};

/// The collector that receives this session's read-certification outcomes.
///
/// The session's workspace registry owns the collector when there is one, and
/// the agent's launch registry is the fallback. That is the same fallback the
/// lifecycle observer uses.
pub(super) fn certification_metrics(agent: &ZedAgent, session: &SessionHandle) -> Arc<MetricsCollector> {
    match session.workspace_runtime() {
        Ok(Some(runtime)) => runtime.local_tool_registry.metrics_collector(),
        _ => agent.local_tool_registry.metrics_collector(),
    }
}

/// Records withheld-version outcomes and emits their bounded warnings.
pub(super) struct ReadCertification {
    metrics: Arc<MetricsCollector>,
}

impl ReadCertification {
    /// Start a certification session that counts into `metrics`.
    pub(super) fn new(metrics: Arc<MetricsCollector>) -> Self {
        Self { metrics }
    }

    /// Count one withheld version and log its fixed operation/outcome labels.
    ///
    /// The warning carries only the outcome category: paths, hashes, patch
    /// text, and error strings are caller-supplied and must stay out of the
    /// diagnostics pipeline.
    fn withheld(&self, outcome: PatchCertificationOutcome, warning: &'static str) {
        self.metrics.record_patch_certification(outcome);
        warn!(operation = "read_file", outcome = outcome.as_str(), "{}", warning);
    }
}

/// Decide the version a read may certify, or `None` when it must be withheld.
///
/// A truncated response never certifies: the returned text is neither the whole
/// file nor the on-disk bytes. A windowed read certifies only when its text
/// equals the on-disk projection; a whole-file read hashes the bytes it
/// returned. Each withheld case is counted under its own outcome.
pub(super) async fn expected_read_hash(
    path: &Path,
    line: Option<u32>,
    limit: Option<u32>,
    returned: &str,
    truncated: bool,
    certification: &ReadCertification,
) -> Option<String> {
    if truncated {
        return None;
    }
    if line.is_none() && limit.is_none() {
        return Some(format!("sha256:{}", diapason_commons::utils::calculate_sha256(returned.as_bytes())));
    }
    match ZedAgent::windowed_disk_hash(path, line, limit, returned).await {
        Ok(Some(content_hash)) => Some(content_hash),
        Ok(None) => {
            certification.withheld(
                PatchCertificationOutcome::WindowProjectionMismatch,
                "ACP read window does not match the on-disk file version",
            );
            None
        }
        Err(WindowVerificationError::Read(_error)) => {
            certification.withheld(
                PatchCertificationOutcome::WindowReadFailed,
                "Failed to read the on-disk file for ACP read version certification",
            );
            None
        }
        Err(WindowVerificationError::NotUtf8) => {
            certification.withheld(
                PatchCertificationOutcome::WindowNotUtf8,
                "On-disk content is not valid UTF-8 for ACP read version certification",
            );
            None
        }
    }
}

/// Record a certified version, returning the model-facing hash or null.
///
/// The file-ops tool confirms the file still holds the certified bytes before
/// returning the hash; a changed file and a failed capture are distinct
/// outcomes so telemetry can tell them apart.
pub(super) async fn capture_read_version(
    agent: &ZedAgent,
    session: &SessionHandle,
    path: &Path,
    expected_hash: &str,
    certification: &ReadCertification,
) -> Value {
    // Select the workspace registry once so the two success arms cannot drift
    // apart. This only reads the file-ops service, so it needs no session-scoped
    // subagent controller.
    let runtime = session.workspace_runtime();
    let registry = match &runtime {
        Ok(Some(runtime)) => &runtime.local_tool_registry,
        Ok(None) => &agent.local_tool_registry,
        Err(_error) => {
            certification.withheld(
                PatchCertificationOutcome::ReadVersionCaptureFailed,
                "Failed to capture ACP read file version",
            );
            return Value::Null;
        }
    };
    let record_result = registry
        .file_ops_tool()
        .record_read_content_hash_if_matches(path, expected_hash)
        .await;
    match record_result {
        Ok(Some(content_hash)) => json!(content_hash),
        Ok(None) => {
            certification.withheld(
                PatchCertificationOutcome::ReadContentMismatch,
                "ACP read content does not match the on-disk file version",
            );
            Value::Null
        }
        Err(_error) => {
            certification.withheld(
                PatchCertificationOutcome::ReadVersionCaptureFailed,
                "Failed to capture ACP read file version",
            );
            Value::Null
        }
    }
}

impl ZedAgent {
    /// Hash the whole on-disk file when a returned line window matches its projection.
    ///
    /// Returns `Ok(None)` when the returned text is not the same window as the
    /// on-disk bytes. A read or UTF-8 error is propagated without including
    /// file content, so the caller can withhold the version and log a bounded
    /// warning.
    async fn windowed_disk_hash(
        path: &Path,
        line: Option<u32>,
        limit: Option<u32>,
        returned: &str,
    ) -> Result<Option<String>, WindowVerificationError> {
        // Keep the error path-free: the caller logs a bounded outcome
        // category, and this error is never shown to the model. Read and
        // UTF-8 failures stay distinct so telemetry can tell them apart.
        let disk_bytes = tokio::fs::read(path).await.map_err(WindowVerificationError::Read)?;
        // The UTF-8 error is deliberately not carried: its `Display` renders the
        // byte offset, and the outcome category alone is what telemetry and the
        // log record. Bind it by name so the discard stays explicit.
        let disk_text = std::str::from_utf8(&disk_bytes).map_err(|_utf8_error| WindowVerificationError::NotUtf8)?;
        let projected = diapason_core::line_window(disk_text, line, limit);
        if !window_matches_projection(projected, returned) {
            return Ok(None);
        }
        Ok(Some(format!("sha256:{}", diapason_commons::utils::calculate_sha256(&disk_bytes))))
    }
}

/// Why a windowed read could not be certified against the on-disk bytes.
///
/// The variants stay distinct so telemetry can count an unreadable file
/// separately from a non-UTF-8 one, and neither variant carries the path or
/// file bytes.
#[derive(Debug)]
enum WindowVerificationError {
    /// The on-disk file could not be read back.
    Read(std::io::Error),
    /// The on-disk bytes were not valid UTF-8.
    NotUtf8,
}

impl std::fmt::Display for WindowVerificationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(_) => formatter.write_str("Failed to read back file for version certification"),
            Self::NotUtf8 => formatter.write_str("File content is not valid UTF-8"),
        }
    }
}

impl std::error::Error for WindowVerificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(error) => Some(error),
            Self::NotUtf8 => None,
        }
    }
}

/// Decide whether a returned read window is the on-disk projection.
///
/// `line_window` keeps line terminators, so the projections it produces end
/// with `\n` unless the file's last line is unterminated. The only tolerated
/// transport difference is a client dropping the terminator of the final line,
/// so `projected` may carry exactly one `\n` more than `returned` — and only
/// when `returned` is itself unterminated. A response that ends with `\n` is a
/// complete set of lines, so a shorter one has dropped a whole line; extra
/// blank lines are retained as well, because a client returning more text than
/// the window may be showing unsaved buffer content. Trimming every trailing
/// newline would accept such a buffer and certify disk bytes the model never
/// saw within its window, which is why the rule is deliberately asymmetric.
fn window_matches_projection(projected: &str, returned: &str) -> bool {
    if projected == returned {
        return true;
    }
    !returned.ends_with('\n') && projected.strip_suffix('\n') == Some(returned)
}

#[cfg(test)]
mod window_match_tests {
    //! Windowed read-certification regressions.
    //!
    //! These exercise `ZedAgent`'s on-disk version certification and the
    //! `window_matches_projection` rule that decides whether a returned window can
    //! be certified. They stay beside the private helpers they cover so the
    //! projection asymmetry and the error-chain hygiene are reviewed with the code
    //! that mints or withholds `content_hash` values.

    use super::{WindowVerificationError, ZedAgent, window_matches_projection};
    use rstest::rstest;

    /// A failed disk read must not carry the file path into its error chain.
    ///
    /// The caller logs a fixed outcome category, and the chain is never shown
    /// to the model, so a path here would be unbounded log context. The
    /// assertion renders the source as well, because `Display` alone is a fixed
    /// string and would pass regardless of what the I/O error carries.
    #[tokio::test]
    async fn windowed_disk_hash_error_omits_the_path() {
        let workspace = assert_fs::TempDir::new().expect("temp dir");
        let path = workspace.path().join("private-notes.txt");
        let error = ZedAgent::windowed_disk_hash(&path, None, None, "")
            .await
            .expect_err("missing file must fail");
        let mut rendered = format!("{error:#}");
        if let Some(source) = std::error::Error::source(&error) {
            rendered.push_str(&format!(": {source}"));
        }
        assert!(!rendered.contains("private-notes.txt"), "error chain leaked the path: {rendered}");
        assert!(
            matches!(error, WindowVerificationError::Read(_)),
            "a missing file must stay a read failure, not a UTF-8 failure: {error:?}"
        );
    }

    /// Non-UTF-8 on-disk bytes get their own outcome so telemetry can tell a
    /// binary file apart from an unreadable one.
    #[tokio::test]
    async fn windowed_disk_hash_reports_non_utf8_separately() {
        let workspace = assert_fs::TempDir::new().expect("temp dir");
        let path = workspace.path().join("binary.bin");
        std::fs::write(&path, [0xff, 0xfe, 0x00]).expect("write non-UTF-8 fixture");
        let error = ZedAgent::windowed_disk_hash(&path, None, None, "")
            .await
            .expect_err("non-UTF-8 content must not be certified");
        assert!(
            matches!(error, WindowVerificationError::NotUtf8),
            "non-UTF-8 content must not be reported as a read failure: {error:?}"
        );
        assert_eq!(error.to_string(), "File content is not valid UTF-8");
        assert!(std::error::Error::source(&error).is_none(), "a UTF-8 failure has no underlying I/O error");
    }

    /// Only a dropped final terminator is tolerated. A shorter complete set of
    /// lines, extra blank lines, and a wider response never match, because each
    /// can carry content the model did not see inside its requested window.
    #[rstest]
    #[case::identical("alpha\n", "alpha\n", true)]
    #[case::both_empty("", "", true)]
    #[case::dropped_final_terminator("alpha\n", "alpha", true)]
    #[case::added_terminator("alpha", "alpha\n", false)]
    #[case::dropped_blank_line("alpha\n\n", "alpha\n", false)]
    #[case::dropped_second_line("alpha\nbeta\n", "alpha\n", false)]
    #[case::one_extra_blank_line("alpha\n", "alpha\n\n", false)]
    #[case::two_extra_blank_lines("alpha\n\n", "alpha\n\n\n", false)]
    #[case::extra_line("alpha\n", "alpha\nbeta\n", false)]
    #[case::wider_response_before("beta\n", "alpha\nbeta\n", false)]
    #[case::wider_response_after("beta\n", "beta\ngamma\n", false)]
    fn window_projection_matching(#[case] projected: &str, #[case] returned: &str, #[case] matches: bool) {
        assert_eq!(
            window_matches_projection(projected, returned),
            matches,
            "projected {projected:?} against returned {returned:?}"
        );
    }
}

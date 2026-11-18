//! Covers optional MCP setup failing closed and reporting bounded diagnostics.
//!
//! A sandbox policy that cannot be constructed must never downgrade to an
//! unsandboxed launch, and the originating detail must not reach default
//! operator diagnostics.

use super::mcp::{McpSandboxPolicySource, attach_acp_mcp_client_with};
use super::{OptionalSetupFailure, OptionalSetupOutcome, report_optional_setup_outcome};
use crate::zed::agent::test_log_capture::{capturing, capturing_async};
use assert_fs::TempDir;
use diapason_config::DiapasonConfig;
use diapason_core::mcp::McpSandboxContext;
use diapason_core::tools::registry::ToolRegistry as CoreToolRegistry;
use std::path::Path;
use tracing_subscriber::filter::LevelFilter;

/// Sandbox-policy construction that always fails with caller-supplied text,
/// standing in for a policy builder that cannot read its workspace.
struct FailingSandboxPolicy(&'static str);

impl McpSandboxPolicySource for FailingSandboxPolicy {
    fn build(&self, _config: &DiapasonConfig, _workspace_root: &Path) -> anyhow::Result<Option<McpSandboxContext>> {
        anyhow::bail!("sandbox policy build failed for {}", self.0)
    }
}

/// A policy that cannot be constructed must fail the component closed: no
/// MCP client is attached, no unsandboxed fallback is started, and the
/// originating detail never reaches default diagnostics.
#[tokio::test]
async fn sandbox_policy_failure_fails_closed_without_raw_detail() {
    let temp = TempDir::new().expect("temporary workspace");
    let registry = CoreToolRegistry::new(temp.path().to_path_buf()).await;
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.mcp.enabled = true;
    let sentinel = "private-sentinel-sandbox-policy";

    let (outcome, warnings) = capturing_async(
        LevelFilter::INFO,
        attach_acp_mcp_client_with(&registry, Some(&vt_cfg), temp.path(), &FailingSandboxPolicy(sentinel)),
    )
    .await;

    assert_eq!(
        outcome,
        OptionalSetupOutcome::Failed(OptionalSetupFailure::McpSandboxPolicyUnavailable),
        "policy construction failure must fail the component closed"
    );
    assert!(registry.mcp_client().is_none(), "the failure must happen before MCP initialization or attachment");

    let attached_warnings = warnings.join("\n");
    assert!(
        !attached_warnings.contains(sentinel),
        "originating detail must stay out of operator-visible logs, got {attached_warnings}"
    );
    assert!(
        !attached_warnings.contains("sandbox policy build failed"),
        "raw error text must stay out of operator-visible logs, got {attached_warnings}"
    );

    // The operator-facing report is emitted by the caller, so assert its
    // bounded fields on the reported category rather than on attach.
    let (_, reported) = capturing(|| report_optional_setup_outcome("MCP client", &outcome));
    let reported = reported.join("\n");
    assert!(reported.contains("operation=\"optional_component_setup\""));
    assert!(reported.contains("component=\"MCP client\""));
    assert!(reported.contains("outcome=\"failed\""));
    assert!(reported.contains("error_category=\"mcp_sandbox_policy_unavailable\""));
    assert!(
        !reported.contains(sentinel),
        "the reported category must not carry the originating detail, got {reported}"
    );
}

/// The suppressed failure logs only fixed fields and a bounded digest at
/// debug; the originating chain text is never written at any level.
#[tokio::test]
async fn sandbox_policy_failure_logs_only_fixed_fields_and_a_digest_at_debug() {
    let temp = TempDir::new().expect("temporary workspace");
    let registry = CoreToolRegistry::new(temp.path().to_path_buf()).await;
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.mcp.enabled = true;
    let sentinel = "private-sentinel-sandbox-debug";

    let (_, debug_events) = capturing_async(
        LevelFilter::DEBUG,
        attach_acp_mcp_client_with(&registry, Some(&vt_cfg), temp.path(), &FailingSandboxPolicy(sentinel)),
    )
    .await;

    let debugged = debug_events.join("\n");
    assert!(debugged.contains("operation=\"optional_component_setup\""));
    assert!(debugged.contains("error_category=\"mcp_sandbox_policy_unavailable\""));
    assert!(debugged.contains("error_bytes="));
    assert!(debugged.contains("detail_correlation="));
    assert!(!debugged.contains(sentinel), "originating detail must never reach logs, got {debugged}");
    assert!(
        !debugged.contains("sandbox policy build failed"),
        "raw error text must never reach logs, got {debugged}"
    );

    let correlation = debugged
        .split("detail_correlation=")
        .nth(1)
        .and_then(|digest| digest.split_whitespace().next())
        .expect("the suppressed failure must log a correlation digest");
    assert_eq!(correlation.len(), 12, "the digest must be bounded, got {correlation}");
    assert!(correlation.chars().all(|c| c.is_ascii_hexdigit()), "the digest must be hex, got {correlation}");
}

/// Degraded outcomes report a bounded category and count, never the joined
/// failure text they replaced.
#[test]
fn degraded_outcome_reports_bounded_categories() {
    let outcome = OptionalSetupOutcome::AttachedWithDegradation(vec![
        OptionalSetupFailure::BackgroundSubagentRestoreFailed,
        OptionalSetupFailure::McpToolRegistrationFailed,
    ]);

    let (_, warnings) = capturing(|| report_optional_setup_outcome("MCP client", &outcome));

    let warned = warnings.join("\n");
    assert!(warned.contains("outcome=\"attached_with_degradation\""));
    assert!(warned.contains("failure_count=2"));
    assert!(warned.contains("error_category=\"background_subagent_restore_failed\""));
    assert!(!warned.contains("failed to restore"), "joined failure text must not return");
}

/// Failed outcomes publish only their stable category code.
#[test]
fn failed_outcome_reports_a_bounded_category() {
    let outcome = OptionalSetupOutcome::Failed(OptionalSetupFailure::McpSandboxPolicyUnavailable);

    let (_, warnings) = capturing(|| report_optional_setup_outcome("MCP client", &outcome));

    let warned = warnings.join("\n");
    assert!(warned.contains("outcome=\"failed\""));
    assert!(warned.contains("error_category=\"mcp_sandbox_policy_unavailable\""));
}

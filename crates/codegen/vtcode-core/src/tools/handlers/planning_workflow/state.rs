//! Shared planning-workflow state across the start/finish tools.
//!
//! This is the only piece of state that spans the tool boundaries; all file
//! I/O and tool wiring live elsewhere (`persistence.rs`, `start.rs`,
//! `finish.rs`). The struct intentionally exposes a narrow, `pub` method
//! surface and keeps its fields private.

use super::artefacts::PlanValidationReport;
use crate::core::agent::types::AgentType;
use crate::metrics::{MetricsCollector, PlanValidationRejectionReason};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

/// Shared state for planning workflow across tools
#[derive(Debug, Clone)]
pub struct PlanningWorkflowState {
    /// Whether planning workflow is currently active
    is_active: Arc<AtomicBool>,
    /// Path to the current plan file (if any)
    current_plan_file: Arc<tokio::sync::RwLock<Option<PathBuf>>>,
    /// Baseline time to require plan updates after initial creation
    plan_baseline: Arc<tokio::sync::RwLock<Option<SystemTime>>>,
    /// Workspace root for plan directory
    workspace_root: PathBuf,
    /// Optional session-owned metrics collector. Standalone state remains inert.
    metrics: Option<Arc<MetricsCollector>>,
}

impl PlanningWorkflowState {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            is_active: Arc::new(AtomicBool::new(false)),
            current_plan_file: Arc::new(tokio::sync::RwLock::new(None)),
            plan_baseline: Arc::new(tokio::sync::RwLock::new(None)),
            workspace_root,
            metrics: None,
        }
    }

    /// Attach the session-owned metrics collector to this planning state.
    pub fn with_metrics(mut self, metrics: Arc<MetricsCollector>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Record a placeholder-token rejection at a definitive validation boundary.
    pub fn record_plan_validation_rejection(&self, report: &PlanValidationReport) {
        if report.placeholder_tokens.is_empty() {
            return;
        }
        if let Some(metrics) = &self.metrics {
            metrics.record_plan_validation_rejection(PlanValidationRejectionReason::PlaceholderToken);
        }
    }

    /// Check if planning workflow is active
    pub fn is_active(&self) -> bool {
        self.is_active.load(Ordering::Relaxed)
    }

    /// Enable planning workflow
    pub fn enable(&self) {
        self.is_active.store(true, Ordering::Relaxed);
    }

    /// Disable planning workflow
    pub fn disable(&self) {
        self.is_active.store(false, Ordering::Relaxed);
    }

    /// Returns the agent type that corresponds to an active planning workflow.
    ///
    /// When planning workflow is active, the effective agent type is `AgentType::Plan`
    /// (the read-only research specialist).  When inactive, returns `None` to signal
    /// the caller should use its own default agent type.
    pub fn effective_agent_type(&self) -> Option<AgentType> {
        if self.is_active() { Some(AgentType::Plan) } else { None }
    }

    /// Get the workspace root path
    pub fn workspace_root(&self) -> Option<PathBuf> {
        if self.workspace_root.as_os_str().is_empty() {
            None
        } else {
            Some(self.workspace_root.clone())
        }
    }

    /// Get the default plans directory path.
    pub fn plans_dir(&self) -> PathBuf {
        if self.workspace_root.as_os_str().is_empty() {
            std::env::temp_dir()
                .join("vtcode-plans")
                .join(workspace_slug_for_tmp(&self.workspace_root))
        } else {
            self.workspace_root.join(".vtcode").join("plans")
        }
    }

    /// Set the current plan file
    pub async fn set_plan_file(&self, path: Option<PathBuf>) {
        let mut guard = self.current_plan_file.write().await;
        *guard = path;
    }

    /// Set the baseline time for plan readiness checks
    pub async fn set_plan_baseline(&self, baseline: Option<SystemTime>) {
        let mut guard = self.plan_baseline.write().await;
        *guard = baseline;
    }

    /// Get the baseline time for plan readiness checks
    pub async fn plan_baseline(&self) -> Option<SystemTime> {
        *self.plan_baseline.read().await
    }

    /// Get the current plan file path
    pub async fn get_plan_file(&self) -> Option<PathBuf> {
        self.current_plan_file.read().await.clone()
    }
}

/// Slugify a workspace root into a filesystem-safe segment for the temp plans dir.
fn workspace_slug_for_tmp(workspace_root: &Path) -> String {
    let fallback = "workspace".to_string();
    let candidate = workspace_root
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .unwrap_or(fallback);
    let sanitized = candidate
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    if sanitized.trim_matches('-').is_empty() {
        "workspace".to_string()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::PlanningWorkflowState;
    use crate::metrics::MetricsCollector;
    use crate::tools::handlers::planning_workflow::artefacts::validate_plan_content;
    use std::path::PathBuf;
    use std::sync::Arc;

    const VALID_PLAN: &str = "# Plan\n\n## Summary\nA concrete plan.\n\n## Implementation Steps\n1. Update src/lib.rs -> files: [src/lib.rs] -> verify: [cargo check]\n\n## Test Cases and Validation\n1. Run cargo check.\n\n## Assumptions and Defaults\n1. Preserve existing behavior.\n";

    #[test]
    fn only_definitive_placeholder_rejections_are_recorded() {
        let collector = Arc::new(MetricsCollector::new());
        let state = PlanningWorkflowState::new(PathBuf::from("/workspace")).with_metrics(collector.clone());
        let legacy = validate_plan_content("[file, symbol, or behavior confirmed from the repo]");

        // Pure validation, such as a history or recovery probe, has no metric effect.
        assert!(!legacy.placeholder_tokens.is_empty());
        assert_eq!(collector.get_planning_metrics().placeholder_token_rejections, 0);

        state.record_plan_validation_rejection(&legacy);
        assert_eq!(collector.get_planning_metrics().placeholder_token_rejections, 1);

        let valid = validate_plan_content(VALID_PLAN);
        assert!(valid.placeholder_tokens.is_empty());
        state.record_plan_validation_rejection(&valid);
        assert_eq!(collector.get_planning_metrics().placeholder_token_rejections, 1);
    }

    #[test]
    fn standalone_state_is_inert_without_a_collector() {
        let state = PlanningWorkflowState::new(PathBuf::new());
        let report = validate_plan_content("[file, symbol, or behavior confirmed from the repo]");
        state.record_plan_validation_rejection(&report);
    }
}

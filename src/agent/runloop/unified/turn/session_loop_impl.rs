use std::io::Write as _;

use anyhow::Result;

use tokio_util::sync::CancellationToken;

use diapason_core::config::loader::DiapasonConfig;
use diapason_core::config::types::AgentConfig as CoreAgentConfig;
use diapason_core::core::agent::steering::SteeringMessage;
use diapason_core::core::interfaces::session::PlanningEntrySource;

/// Optimization: Pre-computed idle detection thresholds to avoid repeated config lookups
#[derive(Clone, Copy)]
struct IdleDetectionConfig {
    timeout_ms: u64,
    backoff_ms: u64,
    max_cycles: usize,
    enabled: bool,
}

use crate::agent::runloop::ResumeSession;

#[path = "session_loop_runner/mod.rs"]
mod session_loop_runner;

const RECENT_MESSAGE_LIMIT: usize = 16;

/// Optimization: Extract idle detection config once to avoid repeated Option unwrapping
#[inline]
fn extract_idle_config(vt_cfg: Option<&DiapasonConfig>) -> IdleDetectionConfig {
    vt_cfg
        .map(|cfg| {
            let idle_config = &cfg.optimization.agent_execution;
            IdleDetectionConfig {
                timeout_ms: idle_config.idle_timeout_ms,
                backoff_ms: idle_config.idle_backoff_ms,
                max_cycles: idle_config.max_idle_cycles,
                enabled: idle_config.idle_timeout_ms > 0,
            }
        })
        .unwrap_or(IdleDetectionConfig {
            timeout_ms: 0,
            backoff_ms: 0,
            max_cycles: 0,
            enabled: false,
        })
}

#[cfg_attr(feature = "profiling", hotpath::measure)]
pub(crate) async fn run_single_agent_loop_unified(
    config: &CoreAgentConfig,
    _vt_cfg: Option<DiapasonConfig>,
    _skip_confirmations: bool,
    full_auto: bool,
    primary_agent_explicitly_configured: bool,
    planning_entry_source: PlanningEntrySource,
    resume: Option<ResumeSession>,
    mut steering_receiver: Option<tokio::sync::mpsc::UnboundedReceiver<SteeringMessage>>,
) -> Result<()> {
    session_loop_runner::run_single_agent_loop_unified_impl(
        config,
        _vt_cfg,
        _skip_confirmations,
        full_auto,
        primary_agent_explicitly_configured,
        planning_entry_source,
        resume,
        &mut steering_receiver,
    )
    .await
}

/// Guard that ensures terminal is restored to a clean state when dropped
/// This handles cases where the TUI doesn't shutdown cleanly or the session
/// exits early (e.g., due to Ctrl+C or other signals)
struct TerminalCleanupGuard;

impl TerminalCleanupGuard {
    fn new() -> Self {
        Self
    }
}

impl Drop for TerminalCleanupGuard {
    fn drop(&mut self) {
        let _ = diapason_ui::tui::panic_hook::restore_tui();
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
    }
}

/// Guard that ensures a CancellationToken is cancelled when dropped
struct CancelGuard(CancellationToken);

impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

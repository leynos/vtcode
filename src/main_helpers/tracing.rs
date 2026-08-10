use anyhow::{Context, Result};
use std::path::Path;
use vtcode_core::utils::error_log_collector::ErrorLogCollectorLayer;
use vtcode_core::utils::session_debug::{
    DEFAULT_MAX_DEBUG_LOG_AGE_DAYS, DEFAULT_MAX_DEBUG_LOG_SIZE_MB, current_debug_session_id, prepare_debug_log_file,
    set_runtime_debug_log_path,
};
use vtcode_core::utils::trace_writer::FlushableWriter;
use vtcode_ui::tui::log::{is_tui_log_capture_enabled, make_tui_log_layer};

fn maybe_tui_log_layer() -> Option<vtcode_ui::tui::log::TuiLogLayer> {
    if is_tui_log_capture_enabled() {
        Some(make_tui_log_layer())
    } else {
        None
    }
}

/// Build the common tracing stack: line-buffered file writer + TUI layer + error collector.
///
/// Returns `Ok(())` on success, or an error if subscriber init fails.
fn install_tracing_stack(log_file: &Path, env_filter: tracing_subscriber::EnvFilter) -> Result<()> {
    use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

    set_runtime_debug_log_path(log_file);
    let writer =
        FlushableWriter::open(log_file).with_context(|| format!("Failed to open trace log: {}", log_file.display()))?;

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(move || writer.clone())
        .with_span_events(FmtSpan::FULL)
        .with_ansi(false);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .with(maybe_tui_log_layer())
        .with(ErrorLogCollectorLayer)
        .try_init()
        .map_err(|err| anyhow::anyhow!("{err}"))
}

pub(crate) async fn initialize_tracing() -> Result<bool> {
    if std::env::var("RUST_LOG").is_ok() {
        let env_filter = tracing_subscriber::EnvFilter::from_default_env();
        let session_id = current_debug_session_id();
        let log_file =
            prepare_debug_log_file(None, &session_id, DEFAULT_MAX_DEBUG_LOG_SIZE_MB, DEFAULT_MAX_DEBUG_LOG_AGE_DAYS)?;

        if let Err(err) = install_tracing_stack(&log_file, env_filter) {
            tracing::warn!(error = %err, "tracing already initialized; skipping env tracing setup");
        }

        return Ok(true);
    }

    Ok(false)
}

pub(crate) fn initialize_default_error_tracing() -> Result<()> {
    use tracing_subscriber::prelude::*;

    let env_filter = tracing_subscriber::EnvFilter::new("error");

    let init_result = tracing_subscriber::registry()
        .with(env_filter)
        .with(ErrorLogCollectorLayer)
        .try_init();

    if let Err(err) = init_result {
        tracing::warn!(error = %err, "tracing already initialized; skipping default error tracing setup");
    }

    Ok(())
}

pub(crate) fn initialize_tracing_from_config(config: &vtcode_core::config::loader::VTCodeConfig) -> Result<()> {
    let debug_cfg = &config.debug;
    let targets = if debug_cfg.trace_targets.is_empty() {
        vec!["vtcode_core", "vtcode"]
    } else {
        debug_cfg.trace_targets.iter().map(String::as_str).collect()
    };
    let targets_display = targets.join(",");
    let filter_str = trace_filter_directives(&targets, debug_cfg.trace_level.as_str());

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&filter_str));

    let configured_dir = debug_cfg.debug_log_dir.as_ref().map(|_| debug_cfg.debug_log_path());
    let session_id = current_debug_session_id();
    let log_file = prepare_debug_log_file(
        configured_dir,
        &session_id,
        debug_cfg.max_debug_log_size_mb,
        debug_cfg.max_debug_log_age_days,
    )?;

    match install_tracing_stack(&log_file, env_filter) {
        Ok(()) => {
            tracing::info!(
                "Debug tracing enabled: targets={}, level={}, log_file={}",
                targets_display,
                debug_cfg.trace_level,
                log_file.display()
            );
        }
        Err(err) => {
            tracing::warn!(
                error = %err,
                "tracing already initialized; skipping config tracing setup"
            );
        }
    }

    Ok(())
}

fn trace_filter_directives(targets: &[&str], level: &str) -> String {
    targets
        .iter()
        .map(|target| format!("{target}={level}"))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::trace_filter_directives;

    #[test]
    fn trace_level_applies_to_every_configured_target() {
        assert_eq!(
            trace_filter_directives(&["vtcode", "vtcode_core", "vtcode_acp"], "debug"),
            "vtcode=debug,vtcode_core=debug,vtcode_acp=debug"
        );
    }
}

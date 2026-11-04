use anyhow::Result;
use diapason_core::config::DiapasonConfig;
use diapason_core::config::types::AgentConfig as CoreAgentConfig;
use diapason_core::review::ReviewSpec;
use std::path::PathBuf;

use super::exec::{ExecCommandKind, ExecCommandOptions};

#[derive(Debug, Clone)]
pub struct ReviewCommandOptions {
    pub json: bool,
    pub events_path: Option<PathBuf>,
    pub last_message_file: Option<PathBuf>,
    pub spec: ReviewSpec,
    pub primary_agent_explicitly_configured: bool,
}

pub async fn handle_review_command(
    config: &CoreAgentConfig,
    vt_cfg: &DiapasonConfig,
    options: ReviewCommandOptions,
) -> Result<()> {
    let exec_options = ExecCommandOptions {
        json: options.json,
        dry_run: false,
        events_path: options.events_path,
        last_message_file: options.last_message_file,
        command: ExecCommandKind::Review { spec: options.spec },
        primary_agent_explicitly_configured: options.primary_agent_explicitly_configured,
    };

    super::exec::handle_exec_command(config, vt_cfg, exec_options).await
}

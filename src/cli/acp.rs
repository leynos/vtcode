//! Validates ACP command configuration and launches the selected protocol adapter.
use anyhow::{Result, bail};
use diapason::acp::{StandardAcpAdapter, ZedAcpAdapter};
use diapason_core::cli::args::AgentClientProtocolTarget;
use diapason_core::config::types::AgentConfig as CoreAgentConfig;
use diapason_core::config::{AgentClientProtocolTransport, DiapasonConfig};
use diapason_core::core::interfaces::acp::{AcpClientAdapter, AcpLaunchParams};

pub async fn handle_acp_command(
    config: &CoreAgentConfig,
    vt_cfg: &DiapasonConfig,
    target: AgentClientProtocolTarget,
    skip_confirmations: bool,
) -> Result<()> {
    handle_acp_command_with_adapters(config, vt_cfg, target, skip_confirmations, &ZedAcpAdapter, &StandardAcpAdapter)
        .await
}

pub(super) async fn handle_acp_command_with_adapters(
    config: &CoreAgentConfig,
    vt_cfg: &DiapasonConfig,
    target: AgentClientProtocolTarget,
    skip_confirmations: bool,
    zed: &dyn AcpClientAdapter,
    standard: &dyn AcpClientAdapter,
) -> Result<()> {
    if !vt_cfg.acp.enabled {
        bail!(
            "Agent Client Protocol integration is disabled. Enable it via [acp] in config.toml or set DIAPASON_ACP_ENABLED=1."
        );
    }

    match target {
        AgentClientProtocolTarget::Zed => {
            if !vt_cfg.acp.zed.enabled {
                bail!(
                    "Zed integration is disabled. Enable it via [acp.zed] in config.toml or set DIAPASON_ACP_ZED_ENABLED=1."
                );
            }

            if vt_cfg.acp.zed.transport != AgentClientProtocolTransport::Stdio {
                bail!("Only the stdio transport is currently supported for Zed ACP integration.");
            }

            let params = AcpLaunchParams::new(config, vt_cfg).with_skip_confirmations(skip_confirmations);
            zed.serve(params).await?
        }
        AgentClientProtocolTarget::Standard => {
            let params = AcpLaunchParams::new(config, vt_cfg).with_skip_confirmations(skip_confirmations);
            standard.serve(params).await?
        }
    }

    Ok(())
}

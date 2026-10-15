//! Validates ACP command configuration and launches the selected protocol adapter.
use anyhow::{Result, bail};
use vtcode::acp::{StandardAcpAdapter, ZedAcpAdapter};
use vtcode_core::cli::args::AgentClientProtocolTarget;
use vtcode_core::config::types::AgentConfig as CoreAgentConfig;
use vtcode_core::config::{AgentClientProtocolTransport, VTCodeConfig};
use vtcode_core::core::interfaces::acp::{AcpClientAdapter, AcpLaunchParams};

pub async fn handle_acp_command(
    config: &CoreAgentConfig,
    vt_cfg: &VTCodeConfig,
    target: AgentClientProtocolTarget,
    skip_confirmations: bool,
) -> Result<()> {
    handle_acp_command_with_adapters(config, vt_cfg, target, skip_confirmations, &ZedAcpAdapter, &StandardAcpAdapter)
        .await
}

async fn handle_acp_command_with_adapters(
    config: &CoreAgentConfig,
    vt_cfg: &VTCodeConfig,
    target: AgentClientProtocolTarget,
    skip_confirmations: bool,
    zed: &dyn AcpClientAdapter,
    standard: &dyn AcpClientAdapter,
) -> Result<()> {
    if !vt_cfg.acp.enabled {
        bail!(
            "Agent Client Protocol integration is disabled. Enable it via [acp] in vtcode.toml or set VT_ACP_ENABLED=1."
        );
    }

    match target {
        AgentClientProtocolTarget::Zed => {
            if !vt_cfg.acp.zed.enabled {
                bail!(
                    "Zed integration is disabled. Enable it via [acp.zed] in vtcode.toml or set VT_ACP_ZED_ENABLED=1."
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use vtcode_core::config::core::PromptCachingConfig;
    use vtcode_core::config::types::{ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference};

    #[derive(Default)]
    struct RecordingAdapter(Mutex<Vec<bool>>);

    #[async_trait::async_trait(?Send)]
    impl AcpClientAdapter for RecordingAdapter {
        async fn serve(&self, params: AcpLaunchParams<'_>) -> Result<()> {
            self.0.lock().expect("recorded launches").push(params.skip_confirmations);
            Ok(())
        }
    }

    fn sample_agent_config() -> CoreAgentConfig {
        CoreAgentConfig {
            model: "test-model".to_owned(),
            api_key: "test-key".to_owned(),
            provider: "test-provider".to_owned(),
            openai_chatgpt_auth: None,
            api_key_env: "TEST_API_KEY".to_owned(),
            workspace: PathBuf::from("."),
            verbose: false,
            quiet: false,
            theme: "default".to_owned(),
            reasoning_effort: ReasoningEffortLevel::default(),
            ui_surface: UiSurfacePreference::default(),
            prompt_cache: PromptCachingConfig::default(),
            model_source: ModelSelectionSource::default(),
            custom_api_keys: BTreeMap::new(),
            checkpointing_enabled: false,
            checkpointing_storage_dir: None,
            checkpointing_max_snapshots: 0,
            checkpointing_max_age_days: None,
            max_conversation_turns: 1,
            model_behaviour: None,
        }
    }

    #[tokio::test]
    async fn cli_forwards_skip_confirmations_to_both_selected_adapters() {
        let agent_config = sample_agent_config();
        let mut config = VTCodeConfig::default();
        config.acp.enabled = true;
        config.acp.zed.enabled = true;
        for target in [AgentClientProtocolTarget::Zed, AgentClientProtocolTarget::Standard] {
            for skip_confirmations in [false, true] {
                let zed = RecordingAdapter::default();
                let standard = RecordingAdapter::default();
                handle_acp_command_with_adapters(&agent_config, &config, target, skip_confirmations, &zed, &standard)
                    .await
                    .expect("CLI adapter launch");
                let (selected, unselected) = match target {
                    AgentClientProtocolTarget::Zed => (&zed, &standard),
                    AgentClientProtocolTarget::Standard => (&standard, &zed),
                };
                assert_eq!(*selected.0.lock().expect("selected adapter"), vec![skip_confirmations]);
                assert!(unselected.0.lock().expect("unselected adapter").is_empty());
            }
        }
    }
}

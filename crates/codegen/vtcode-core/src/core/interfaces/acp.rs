//! Defines the launch parameters and adapter port used by hosts to start an ACP client.
use anyhow::Result;
use async_trait::async_trait;

use crate::config::loader::VTCodeConfig;
use crate::config::types::AgentConfig as CoreAgentConfig;

/// Parameters required to launch an Agent Client Protocol adapter.
#[derive(Debug)]
pub struct AcpLaunchParams<'a> {
    /// Agent configuration used to initialize the ACP runtime.
    pub agent_config: &'a CoreAgentConfig,
    /// Runtime configuration supplying ACP and provider settings.
    pub runtime_config: &'a VTCodeConfig,
    /// Whether ordinary tool confirmation prompts should be bypassed.
    pub skip_confirmations: bool,
}

impl<'a> AcpLaunchParams<'a> {
    /// Create launch parameters with ordinary permission confirmations enabled.
    pub fn new(agent_config: &'a CoreAgentConfig, runtime_config: &'a VTCodeConfig) -> Self {
        Self {
            agent_config,
            runtime_config,
            skip_confirmations: false,
        }
    }

    #[must_use]
    /// Set whether ordinary tool permission confirmations are bypassed.
    ///
    /// Forced permission requests, such as lifecycle policy checks, may still
    /// prompt the client.
    pub fn with_skip_confirmations(mut self, skip_confirmations: bool) -> Self {
        self.skip_confirmations = skip_confirmations;
        self
    }
}

/// Interface for components that expose VT Code over the Agent Client Protocol.
#[async_trait(?Send)]
pub trait AcpClientAdapter: Send + Sync {
    /// Serve the ACP client using the supplied agent and runtime configuration.
    async fn serve(&self, params: AcpLaunchParams<'_>) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::AcpLaunchParams;
    use crate::config::core::PromptCachingConfig;
    use crate::config::loader::VTCodeConfig;
    use crate::config::types::{AgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference};

    fn sample_agent_config() -> AgentConfig {
        AgentConfig {
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

    #[test]
    fn skip_confirmations_builder_sets_public_launch_parameter() {
        let agent_config = sample_agent_config();
        let runtime_config = VTCodeConfig::default();

        let params = AcpLaunchParams::new(&agent_config, &runtime_config).with_skip_confirmations(true);

        assert!(params.skip_confirmations);
    }
}

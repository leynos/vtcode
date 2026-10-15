//! Adapts the core ACP launch interface to the Zed protocol connection and agent implementation.
use anyhow::Result;
use async_trait::async_trait;
use vtcode_core::core::interfaces::acp::{AcpClientAdapter, AcpLaunchParams};

mod agent;
pub(crate) mod connection;
pub(crate) mod constants;
mod helpers;
mod provider_runtime;
mod session;
mod types;
mod workspace_policy;

pub(crate) use workspace_policy::AcpWorkspacePolicy;

use session::run_acp_agent;

#[derive(Debug, Default, Clone, Copy)]
pub struct ZedAcpAdapter;

#[async_trait(?Send)]
impl AcpClientAdapter for ZedAcpAdapter {
    async fn serve(&self, params: AcpLaunchParams<'_>) -> Result<()> {
        run_acp_agent(params.agent_config, params.runtime_config, Some("Zed".to_string()), params.skip_confirmations)
            .await
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StandardAcpAdapter;

#[async_trait(?Send)]
impl AcpClientAdapter for StandardAcpAdapter {
    async fn serve(&self, params: AcpLaunchParams<'_>) -> Result<()> {
        run_acp_agent(params.agent_config, params.runtime_config, None, params.skip_confirmations).await
    }
}

#[cfg(test)]
#[allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
#[path = "tests/mod.rs"]
mod tests;

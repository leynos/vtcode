use anyhow::Result;
use diapason_core::config::DiapasonConfig;
use diapason_core::config::types::AgentConfig as CoreAgentConfig;
use std::future::Future;

pub fn handle_app_server_command<'a>(
    _agent_config: &'a CoreAgentConfig,
    vt_config: &'a DiapasonConfig,
    listen: &'a str,
) -> impl Future<Output = Result<()>> + 'a {
    crate::codex_app_server::launch_app_server_proxy(Some(vt_config), listen)
}

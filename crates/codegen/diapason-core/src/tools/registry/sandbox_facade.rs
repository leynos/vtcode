//! Sandbox configuration accessors for ToolRegistry.

use super::ToolRegistry;

pub(super) fn runtime_sandbox_config_default() -> diapason_config::SandboxConfig {
    // Keep legacy behaviour for registry instances that never receive workspace config.
    diapason_config::SandboxConfig { enabled: false, ..Default::default() }
}

impl ToolRegistry {
    pub fn apply_sandbox_config(&self, sandbox_config: &diapason_config::SandboxConfig) {
        if let Ok(mut guard) = self.runtime_sandbox_config.write() {
            *guard = sandbox_config.clone();
        }
    }

    pub fn sandbox_config(&self) -> diapason_config::SandboxConfig {
        self.runtime_sandbox_config
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| runtime_sandbox_config_default())
    }
}

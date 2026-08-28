use std::sync::Arc;

use super::ToolRegistry;
use crate::subagents::SubagentController;

impl ToolRegistry {
    /// Shares workspace resources while isolating delegated-agent ownership.
    pub fn with_subagent_controller(&self, controller: Option<Arc<SubagentController>>) -> Self {
        let mut registry = self.clone();
        registry.subagent_controller = Arc::new(std::sync::RwLock::new(controller));
        registry
    }

    pub fn set_subagent_controller(&self, controller: Arc<SubagentController>) {
        if let Ok(mut slot) = self.subagent_controller.write() {
            *slot = Some(controller);
        }
    }

    pub fn subagent_controller(&self) -> Option<Arc<SubagentController>> {
        self.subagent_controller.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn has_subagent_controller(&self) -> bool {
        self.subagent_controller().is_some()
    }
}

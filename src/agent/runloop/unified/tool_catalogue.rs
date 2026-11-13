//! Exposes the session tool-catalogue state alias and change-notifier adapter.

use std::sync::Arc;

pub(crate) type ToolCatalogueState = diapason_core::tools::registry::SessionToolCatalogueState;

/// Build a change-notifier closure backed by the session tool-catalogue state.
pub(crate) fn tool_catalogue_change_notifier(
    tool_catalogue: &Arc<ToolCatalogueState>,
) -> Arc<dyn Fn(&'static str) + Send + Sync> {
    tool_catalogue.change_notifier()
}

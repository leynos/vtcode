use std::sync::Arc;

pub(crate) type ToolCatalogState = diapason_core::tools::registry::SessionToolCatalogState;

pub(crate) fn tool_catalog_change_notifier(
    tool_catalog: &Arc<ToolCatalogState>,
) -> Arc<dyn Fn(&'static str) + Send + Sync> {
    tool_catalog.change_notifier()
}

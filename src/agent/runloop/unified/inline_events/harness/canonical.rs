use anyhow::Result;
use std::path::Path;

use diapason_core::core::agent::events::SessionStoreSink;
use diapason_core::exec::events::ThreadEvent;

/// Authoritative session-event sink for one run.
pub(crate) struct CanonicalEventSink {
    inner: SessionStoreSink,
}

impl CanonicalEventSink {
    /// Open the workspace-local canonical session store.
    pub(crate) async fn open(workspace: &Path, session_id: &str) -> Result<Self> {
        Ok(Self {
            inner: SessionStoreSink::open(workspace, session_id).await?,
        })
    }

    /// Enqueue one event while preserving the caller's order.
    pub(crate) fn emit(&self, event: &ThreadEvent) -> Result<()> {
        self.inner.emit(event)
    }

    /// Drain all accepted events and report persistence failures.
    pub(crate) async fn close(&self) -> Result<()> {
        self.inner.close().await
    }
}

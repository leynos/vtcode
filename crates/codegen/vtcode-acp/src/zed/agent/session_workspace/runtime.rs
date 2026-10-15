//! Workspace runtime lifecycle and cache state shared by ACP session setup.
use super::super::warn_workspace_mcp_shutdown_failure;
use hashbrown::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{Notify, OnceCell};
use vtcode_core::tools::file_ops::FileOpsTool;
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;

use super::super::OptionalWorkspaceSetupOutcomes;
use crate::permissions::AcpPermissionPrompter;
use crate::tooling::AcpToolRegistry;
use crate::zed::helpers::PrimaryAgentCatalog;

pub(in crate::zed) struct SessionWorkspaceRuntime {
    pub(in crate::zed::agent) workspace_root: PathBuf,
    pub(in crate::zed::agent) workspace_hooks_gated: bool,
    pub(in crate::zed::agent) system_prompt: String,
    pub(in crate::zed::agent) primary_agents: PrimaryAgentCatalog,
    pub(in crate::zed::agent) acp_tool_registry: Arc<AcpToolRegistry>,
    pub(in crate::zed::agent) permission_prompter: Arc<dyn AcpPermissionPrompter + Send + Sync>,
    pub(in crate::zed::agent) local_tool_registry: CoreToolRegistry,
    pub(in crate::zed::agent) file_ops_tool: Option<FileOpsTool>,
    pub(in crate::zed::agent) optional_setup_outcomes: OptionalWorkspaceSetupOutcomes,
    pub(in crate::zed::agent) shutdown_managed: bool,
    pub(in crate::zed::agent) lifecycle: Mutex<WorkspaceRuntimeLifecycle>,
    pub(in crate::zed::agent) lifecycle_changed: Notify,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::zed::agent) enum WorkspaceRuntimeLifecycleState {
    Open,
    ShutdownPending,
    ShuttingDown,
    Closed,
}

pub(in crate::zed::agent) struct WorkspaceRuntimeLifecycle {
    pub(in crate::zed::agent) state: WorkspaceRuntimeLifecycleState,
    pub(in crate::zed::agent) active_sessions: usize,
}

pub(in crate::zed) struct SessionWorkspaceLease {
    runtime: Arc<SessionWorkspaceRuntime>,
    counted: bool,
    released: bool,
}

impl SessionWorkspaceLease {
    pub(super) fn unmanaged(runtime: Arc<SessionWorkspaceRuntime>) -> Self {
        Self { runtime, counted: false, released: false }
    }

    pub(super) async fn acquire(runtime: Arc<SessionWorkspaceRuntime>) -> anyhow::Result<Option<Self>> {
        if !runtime.shutdown_managed {
            return Ok(Some(Self::unmanaged(runtime)));
        }

        loop {
            let notified = runtime.lifecycle_changed.notified();
            tokio::pin!(notified);
            let _was_newly_enabled = notified.as_mut().enable();
            let action = {
                let mut lifecycle = runtime
                    .lifecycle
                    .lock()
                    .map_err(|error| anyhow::anyhow!("locking ACP workspace runtime lifecycle: {error}"))?;
                match lifecycle.state {
                    WorkspaceRuntimeLifecycleState::Open => {
                        lifecycle.active_sessions = lifecycle.active_sessions.saturating_add(1);
                        0
                    }
                    WorkspaceRuntimeLifecycleState::ShutdownPending => {
                        lifecycle.state = WorkspaceRuntimeLifecycleState::ShuttingDown;
                        1
                    }
                    WorkspaceRuntimeLifecycleState::ShuttingDown => 2,
                    WorkspaceRuntimeLifecycleState::Closed => 3,
                }
            };

            match action {
                0 => {
                    return Ok(Some(Self {
                        runtime: Arc::clone(&runtime),
                        counted: true,
                        released: false,
                    }));
                }
                1 => runtime.spawn_shutdown(),
                2 => {}
                _ => return Ok(None),
            }
            notified.await;
        }
    }

    pub(in crate::zed::agent) fn runtime(&self) -> &Arc<SessionWorkspaceRuntime> {
        &self.runtime
    }

    pub(in crate::zed) async fn release(mut self) {
        if !self.counted || self.released {
            return;
        }
        self.released = true;
        let last_session = match self.runtime.lifecycle.lock() {
            Ok(mut lifecycle) => {
                lifecycle.active_sessions = lifecycle.active_sessions.saturating_sub(1);
                if lifecycle.active_sessions == 0 && lifecycle.state == WorkspaceRuntimeLifecycleState::Open {
                    lifecycle.state = WorkspaceRuntimeLifecycleState::ShutdownPending;
                    true
                } else {
                    false
                }
            }
            Err(error) => {
                tracing::error!(%error, "Failed to lock ACP workspace runtime lifecycle during session release");
                false
            }
        };
        if last_session {
            self.runtime.lifecycle_changed.notify_waiters();
            self.runtime.finish_pending_shutdown().await;
        }
    }
}

impl SessionWorkspaceRuntime {
    async fn finish_pending_shutdown(self: &Arc<Self>) {
        loop {
            let notified = self.lifecycle_changed.notified();
            tokio::pin!(notified);
            let _was_newly_enabled = notified.as_mut().enable();
            let should_start_shutdown = match self.lifecycle.lock() {
                Ok(mut lifecycle) => match lifecycle.state {
                    WorkspaceRuntimeLifecycleState::ShutdownPending => {
                        lifecycle.state = WorkspaceRuntimeLifecycleState::ShuttingDown;
                        true
                    }
                    WorkspaceRuntimeLifecycleState::ShuttingDown => false,
                    WorkspaceRuntimeLifecycleState::Open | WorkspaceRuntimeLifecycleState::Closed => return,
                },
                Err(error) => {
                    tracing::error!(%error, "Failed to lock ACP workspace runtime lifecycle during shutdown");
                    return;
                }
            };
            if should_start_shutdown {
                self.spawn_shutdown();
            }
            notified.await;
        }
    }

    fn spawn_shutdown(self: &Arc<Self>) {
        let runtime = Arc::clone(self);
        drop(tokio::spawn(async move {
            if let Some(controller) = runtime.local_tool_registry.subagent_controller() {
                controller.signal_shutdown().await;
            }
            if let Some(client) = runtime.local_tool_registry.mcp_client()
                && let Err(error) = client.shutdown().await
            {
                warn_workspace_mcp_shutdown_failure(&error);
            }
            runtime.local_tool_registry.clear_mcp_client().await;
            runtime.mark_shutdown_complete();
            runtime.lifecycle_changed.notify_waiters();
        }));
    }

    fn mark_shutdown_complete(&self) {
        match self.lifecycle.lock() {
            Ok(mut lifecycle) => lifecycle.state = WorkspaceRuntimeLifecycleState::Closed,
            Err(error) => error.into_inner().state = WorkspaceRuntimeLifecycleState::Closed,
        }
    }

    #[cfg(test)]
    pub(in crate::zed::agent) fn is_shutdown(&self) -> bool {
        self.lifecycle
            .lock()
            .map(|lifecycle| lifecycle.state == WorkspaceRuntimeLifecycleState::Closed)
            .unwrap_or_else(|error| error.into_inner().state == WorkspaceRuntimeLifecycleState::Closed)
    }
}

impl Drop for SessionWorkspaceLease {
    fn drop(&mut self) {
        if !self.counted || self.released {
            return;
        }
        let pending = match self.runtime.lifecycle.lock() {
            Ok(mut lifecycle) => {
                lifecycle.active_sessions = lifecycle.active_sessions.saturating_sub(1);
                if lifecycle.active_sessions == 0 && lifecycle.state == WorkspaceRuntimeLifecycleState::Open {
                    lifecycle.state = WorkspaceRuntimeLifecycleState::ShutdownPending;
                    true
                } else {
                    false
                }
            }
            Err(error) => {
                tracing::error!(%error, "Failed to lock ACP workspace runtime lifecycle while dropping a session lease");
                false
            }
        };
        if pending {
            self.runtime.lifecycle_changed.notify_waiters();
        }
    }
}

pub(in crate::zed::agent) struct WorkspaceRuntimeCache<T> {
    pub(super) runtimes: HashMap<PathBuf, Weak<T>>,
    pub(super) in_flight: HashMap<PathBuf, Weak<OnceCell<Arc<T>>>>,
    pub(super) metrics: Arc<vtcode_core::metrics::MetricsCollector>,
}

pub(super) enum WorkspaceRuntimeLookup<T> {
    Ready(Arc<T>),
    Initialize(Arc<OnceCell<Arc<T>>>),
}

impl<T> WorkspaceRuntimeCache<T> {
    pub(in crate::zed::agent) fn new(metrics: Arc<vtcode_core::metrics::MetricsCollector>) -> Self {
        Self {
            runtimes: HashMap::new(),
            in_flight: HashMap::new(),
            metrics,
        }
    }

    pub(in crate::zed::agent) fn remember_runtime(&mut self, workspace_root: PathBuf, runtime: &Arc<T>) {
        drop(self.runtimes.insert(workspace_root, Arc::downgrade(runtime)));
        self.prune_stale_entries();
    }

    pub(super) fn lookup(&mut self, workspace_root: &Path) -> WorkspaceRuntimeLookup<T> {
        self.prune_stale_entries();
        if let Some(runtime) = self.runtimes.get(workspace_root).and_then(Weak::upgrade) {
            return WorkspaceRuntimeLookup::Ready(runtime);
        }
        if let Some(runtime_cell) = self.in_flight.get(workspace_root).and_then(Weak::upgrade) {
            return WorkspaceRuntimeLookup::Initialize(runtime_cell);
        }

        let runtime_cell = Arc::new(OnceCell::new());
        drop(
            self.in_flight
                .insert(workspace_root.to_path_buf(), Arc::downgrade(&runtime_cell)),
        );
        self.update_cache_size_metric();
        WorkspaceRuntimeLookup::Initialize(runtime_cell)
    }

    pub(super) fn store_initialized_runtime(
        &mut self,
        workspace_root: PathBuf,
        runtime: &Arc<T>,
        runtime_cell: &Arc<OnceCell<Arc<T>>>,
    ) {
        self.remember_runtime(workspace_root.clone(), runtime);
        if self
            .in_flight
            .get(&workspace_root)
            .and_then(Weak::upgrade)
            .is_some_and(|cached_cell| Arc::ptr_eq(&cached_cell, runtime_cell))
        {
            drop(self.in_flight.remove(&workspace_root));
        }
        self.prune_stale_entries();
    }

    pub(super) fn remove_runtime_if_same(&mut self, workspace_root: &Path, runtime: &Arc<T>) {
        if self
            .runtimes
            .get(workspace_root)
            .and_then(Weak::upgrade)
            .is_some_and(|cached_runtime| Arc::ptr_eq(&cached_runtime, runtime))
        {
            drop(self.runtimes.remove(workspace_root));
        }
        self.update_cache_size_metric();
    }

    pub(super) fn remove_in_flight_if_same(&mut self, workspace_root: &Path, runtime_cell: &Arc<OnceCell<Arc<T>>>) {
        if self
            .in_flight
            .get(workspace_root)
            .and_then(Weak::upgrade)
            .is_some_and(|cached_cell| Arc::ptr_eq(&cached_cell, runtime_cell))
        {
            drop(self.in_flight.remove(workspace_root));
        }
        self.update_cache_size_metric();
    }

    fn prune_stale_entries(&mut self) {
        self.runtimes.retain(|_, runtime| runtime.strong_count() > 0);
        self.in_flight.retain(|_, runtime_cell| runtime_cell.strong_count() > 0);
        self.update_cache_size_metric();
    }

    fn update_cache_size_metric(&self) {
        self.metrics
            .set_acp_workspace_runtime_cache_entries(self.runtimes.len().saturating_add(self.in_flight.len()));
    }
}

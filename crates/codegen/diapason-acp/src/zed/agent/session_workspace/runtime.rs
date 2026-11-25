//! Workspace runtime lifecycle and cache state shared by ACP session setup.
use super::super::warn_workspace_mcp_shutdown_failure;
use diapason_core::metrics::RuntimeCacheKind;
use diapason_core::tools::file_ops::FileOpsTool;
use diapason_core::tools::registry::ToolRegistry as CoreToolRegistry;
use hashbrown::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{Notify, OnceCell};

use super::super::OptionalWorkspaceSetupOutcomes;
use crate::permissions::AcpPermissionPrompter;
use crate::tooling::AcpToolRegistry;
use crate::zed::helpers::PrimaryAgentCatalogue;

pub(in crate::zed) struct SessionWorkspaceRuntime {
    pub(in crate::zed::agent) workspace_root: PathBuf,
    pub(in crate::zed::agent) workspace_hooks_gated: bool,
    pub(in crate::zed::agent) system_prompt: String,
    pub(in crate::zed::agent) primary_agents: PrimaryAgentCatalogue,
    pub(in crate::zed::agent) acp_tool_registry: Arc<AcpToolRegistry>,
    pub(in crate::zed::agent) permission_prompter: Arc<dyn AcpPermissionPrompter + Send + Sync>,
    pub(in crate::zed::agent) local_tool_registry: CoreToolRegistry,
    pub(in crate::zed::agent) file_ops_tool: Option<FileOpsTool>,
    pub(in crate::zed::agent) optional_setup_outcomes: OptionalWorkspaceSetupOutcomes,
    pub(in crate::zed::agent) shutdown_managed: bool,
    pub(in crate::zed::agent) lifecycle: Mutex<WorkspaceRuntimeLifecycle>,
    pub(in crate::zed::agent) lifecycle_changed: Notify,
    /// The workspace-scope restore of ownerless background records this
    /// runtime still owes, if any.
    pub(in crate::zed::agent) pending_background_restore: Mutex<PendingWorkspaceRestore>,
}

/// What a runtime still owes of the workspace-scope background restore.
///
/// A runtime built while a resumed session's owner migration was unresolved
/// skips restoring ownerless records, because the rows that session launched
/// are still ownerless and a restore would relaunch them as workspace work. The
/// skip must not be permanent, though: the restore is owed once ownership is
/// reconciled, and a later acquisition of the same runtime is the only place
/// that can still perform it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::zed::agent) enum PendingWorkspaceRestore {
    /// Nothing is owed: the build either restored at once or never deferred.
    NotOwed,
    /// Owed, but a resumed session's owner migration is unresolved, so a
    /// restore could relaunch a row that session may yet adopt.
    BlockedByOwnerMigration,
    /// Owed and safe to run: no owner migration is unresolved.
    Ready,
}

impl PendingWorkspaceRestore {
    /// Whether this runtime is holding back an owed restore.
    pub(in crate::zed::agent) fn is_blocked(self) -> bool {
        matches!(self, Self::BlockedByOwnerMigration)
    }

    /// The state after a caller establishes that ownership is reconciled.
    ///
    /// Only a blocked restore changes: a runtime that never deferred has
    /// nothing to run, and one already payable is not re-armed, so the restore
    /// still runs at most once.
    fn released(self) -> Self {
        if self.is_blocked() { Self::Ready } else { self }
    }

    /// The state after a caller claims the owed restore, and whether it ran.
    ///
    /// A claim clears the debt in the same step that reports it, so a runtime
    /// hands its restore to exactly one caller however many sessions acquire it.
    fn claimed(self) -> (Self, bool) {
        match self {
            Self::Ready => (Self::NotOwed, true),
            Self::NotOwed | Self::BlockedByOwnerMigration => (self, false),
        }
    }
}

impl SessionWorkspaceRuntime {
    /// Records that a blocked restore is now safe to run.
    pub(in crate::zed::agent) fn release_background_restore(&self) {
        let mut pending = self.pending_restore_guard();
        *pending = pending.released();
    }

    /// Claims the owed restore, leaving nothing owed.
    ///
    /// The claim and the clear happen under one lock, so a runtime whose
    /// restore is owed hands it to exactly one caller however many sessions
    /// acquire the runtime afterwards.
    pub(in crate::zed::agent) fn take_owed_background_restore(&self) -> bool {
        let mut pending = self.pending_restore_guard();
        let (state, claimed) = pending.claimed();
        *pending = state;
        claimed
    }

    /// Whether a deferred restore is still owed and held back.
    ///
    /// Test-visible: production code never needs to ask, because the claim and
    /// the release are the only decisions taken on this state.
    #[cfg(test)]
    pub(in crate::zed::agent) fn pending_background_restore_is_blocked(&self) -> bool {
        self.pending_restore_guard().is_blocked()
    }

    /// Locks the pending-restore state.
    ///
    /// A poisoned lock still holds a valid `PendingWorkspaceRestore`, and this
    /// state is a plain three-value flag, so recovering the value is safe and
    /// keeps a poisoned lock from silently wedging the debt forever.
    fn pending_restore_guard(&self) -> std::sync::MutexGuard<'_, PendingWorkspaceRestore> {
        self.pending_background_restore
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

    pub(in crate::zed::agent) async fn acquire(runtime: Arc<SessionWorkspaceRuntime>) -> anyhow::Result<Option<Self>> {
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

    #[cfg(test)]
    pub(in crate::zed::agent) fn is_counted(&self) -> bool {
        self.counted
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

    pub(in crate::zed::agent) fn spawn_shutdown(self: &Arc<Self>) {
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

    pub(in crate::zed::agent) fn mark_shutdown_complete(&self) {
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
    pub(super) metrics: Arc<diapason_core::metrics::MetricsCollector>,
}

pub(super) enum WorkspaceRuntimeLookup<T> {
    Ready(Arc<T>),
    Initialize(Arc<OnceCell<Arc<T>>>),
}

impl<T> WorkspaceRuntimeCache<T> {
    pub(in crate::zed::agent) fn new(metrics: Arc<diapason_core::metrics::MetricsCollector>) -> Self {
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

    /// Reports what the cache currently holds for `workspace_root` without changing it.
    ///
    /// This is a pure query: it neither prunes stale entries, publishes the cache-size
    /// metric, nor reserves an initialization slot on a miss. Use
    /// [`Self::lookup_or_reserve_initialization`] when the caller intends to act on a
    /// miss.
    pub(super) fn cached_runtime(&self, workspace_root: &Path) -> Option<WorkspaceRuntimeLookup<T>> {
        if let Some(runtime) = self.runtimes.get(workspace_root).and_then(Weak::upgrade) {
            return Some(WorkspaceRuntimeLookup::Ready(runtime));
        }
        let runtime_cell = self.in_flight.get(workspace_root).and_then(Weak::upgrade)?;
        Some(WorkspaceRuntimeLookup::Initialize(runtime_cell))
    }

    /// Resolves `workspace_root`, reserving an initialization slot on a cache miss.
    ///
    /// This is a command, not a query: it prunes expired entries, publishes the
    /// cache-size metric, and inserts a fresh `OnceCell` into `in_flight` when no
    /// runtime is cached. Call it only from paths that will go on to initialize and
    /// store the runtime; a caller that merely wants to observe the cache should use
    /// [`Self::cached_runtime`].
    pub(super) fn lookup_or_reserve_initialization(&mut self, workspace_root: &Path) -> WorkspaceRuntimeLookup<T> {
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
        self.metrics.set_runtime_cache_entries(
            RuntimeCacheKind::Workspace,
            self.runtimes.len().saturating_add(self.in_flight.len()),
        );
    }
}

#[cfg(test)]
mod pending_restore_tests {
    use super::PendingWorkspaceRestore;

    /// A deferred restore is owed, released once, and claimed exactly once.
    ///
    /// The debt exists so a skipped restore is postponed rather than dropped:
    /// release makes it payable, one claim runs it, and every later claim finds
    /// nothing. A debt claimable twice would restore twice, which is the
    /// duplicate launch the deferral exists to prevent.
    #[test]
    fn a_deferred_restore_is_released_once_and_claimed_once() {
        let blocked = PendingWorkspaceRestore::BlockedByOwnerMigration;
        assert!(blocked.is_blocked(), "a deferred build holds the restore back");
        assert_eq!(blocked.claimed(), (blocked, false), "a blocked restore is not payable yet");

        let ready = blocked.released();
        assert_eq!(ready, PendingWorkspaceRestore::Ready, "releasing makes the debt payable");
        assert_eq!(
            ready.claimed(),
            (PendingWorkspaceRestore::NotOwed, true),
            "the first claim runs the restore and clears the debt"
        );
        assert_eq!(
            PendingWorkspaceRestore::NotOwed.claimed(),
            (PendingWorkspaceRestore::NotOwed, false),
            "a claimed debt must not run a second time"
        );
        assert_eq!(
            PendingWorkspaceRestore::NotOwed.released(),
            PendingWorkspaceRestore::NotOwed,
            "releasing a claimed debt must not re-arm it"
        );
    }

    /// A runtime that restored at build time owes nothing, forever.
    ///
    /// Only a deferred build creates the debt. Releasing a runtime that never
    /// deferred must not arm one, or ordinary `session/new` traffic would run a
    /// legacy restore it never deferred.
    #[test]
    fn a_runtime_that_never_deferred_never_owes_a_restore() {
        assert_eq!(
            PendingWorkspaceRestore::NotOwed.released(),
            PendingWorkspaceRestore::NotOwed,
            "there is nothing to release"
        );
        assert_eq!(
            PendingWorkspaceRestore::NotOwed.claimed(),
            (PendingWorkspaceRestore::NotOwed, false),
            "there is nothing to claim"
        );
    }

    /// Releasing an already-payable restore leaves it single-claim.
    ///
    /// Two acquisitions can both observe an unresolved migration before either
    /// releases it, so a second release is expected traffic and must not make
    /// the restore payable twice.
    #[test]
    fn a_second_release_does_not_make_the_restore_payable_twice() {
        let ready = PendingWorkspaceRestore::BlockedByOwnerMigration.released().released();
        assert_eq!(ready, PendingWorkspaceRestore::Ready, "two releases leave one payable debt");

        // `claimed` is a pure transition and `take_owed_background_restore`
        // stores the state it returns, so a second claim must be taken against
        // the state the first one left behind.
        let (paid, ran) = ready.claimed();
        assert_eq!((paid, ran), (PendingWorkspaceRestore::NotOwed, true), "one claim pays the released debt");
        assert_eq!(
            paid.claimed(),
            (PendingWorkspaceRestore::NotOwed, false),
            "the second release must not create a second debt"
        );
    }
}

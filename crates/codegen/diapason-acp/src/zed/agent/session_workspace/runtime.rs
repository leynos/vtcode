//! Workspace runtime lifecycle and cache state shared by ACP session setup.
use super::super::warn_workspace_mcp_shutdown_failure;
use diapason_core::metrics::RuntimeCacheKind;
use diapason_core::tools::file_ops::FileOpsTool;
use diapason_core::tools::registry::ToolRegistry as CoreToolRegistry;
use hashbrown::HashMap;
use std::collections::BTreeSet;
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
/// A runtime built for a resumed attach skips restoring ownerless records,
/// because the rows that session launched may still be ownerless and a restore
/// would relaunch them as workspace work. The skip must not be permanent: the
/// restore is owed once ownership is reconciled, and only a caller that
/// establishes reconciliation may release it. The debt is discharged only by a
/// restore that ran, so a failed pass is retried by a later acquisition instead
/// of being lost.
///
/// Ownership is reconciled per session. A resumed attach records itself as a
/// blocker until it can show the rows it launched carry their owner, and a
/// release clears only that session's blocker. One session's success therefore
/// cannot make the shared restore payable while another session's rows are
/// still ownerless, which would let the workspace scope relaunch them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::zed::agent) enum PendingWorkspaceRestore {
    /// Nothing is owed: the build either restored at once or never deferred.
    NotOwed,
    /// Owed, but not yet payable: the named resumed attaches have not both
    /// passed their required checkpoint and established that the rows they
    /// launched carry their owner, so a restore could relaunch work a session
    /// may still adopt.
    BlockedByOwnerMigration(BTreeSet<String>),
    /// Owed and safe to run: every resumed attach that deferred it reconciled
    /// ownership after its required checkpoint.
    Ready,
}

impl PendingWorkspaceRestore {
    /// Whether this runtime is holding back an owed restore.
    pub(in crate::zed::agent) fn is_blocked(&self) -> bool {
        matches!(self, Self::BlockedByOwnerMigration(_))
    }

    /// The state a deferred build starts in, blocked by `session_id`.
    pub(super) fn deferred_for(session_id: &str) -> Self {
        Self::blocked_by(&[session_id])
    }

    /// A blocked state whose restore is held by `sessions`.
    fn blocked_by(sessions: &[&str]) -> Self {
        Self::BlockedByOwnerMigration(sessions.iter().map(|session| (*session).to_owned()).collect())
    }

    /// Records that `session_id` still has unresolved ownership.
    ///
    /// A runtime that never deferred owes nothing, so a later resume cannot arm
    /// a restore the build already ran. A payable debt becomes blocked again
    /// until this session reconciles too, because the release that made it
    /// payable says nothing about this session's rows.
    fn block(&mut self, session_id: &str) {
        let state = std::mem::replace(self, Self::NotOwed);
        *self = match state {
            Self::NotOwed => Self::NotOwed,
            Self::BlockedByOwnerMigration(mut unresolved) => {
                let _inserted = unresolved.insert(session_id.to_owned());
                Self::BlockedByOwnerMigration(unresolved)
            }
            Self::Ready => Self::deferred_for(session_id),
        };
    }

    /// The state after `session_id` establishes that ownership is reconciled.
    ///
    /// Only the caller's own blocker is cleared: a restore still owed for
    /// another session's ownerless rows stays blocked, and a runtime that never
    /// deferred is not re-armed.
    fn release(&mut self, session_id: &str) {
        let mut unresolved = match self {
            Self::BlockedByOwnerMigration(unresolved) => unresolved.clone(),
            Self::NotOwed | Self::Ready => return,
        };
        let _removed = unresolved.remove(session_id);
        *self = if unresolved.is_empty() {
            Self::Ready
        } else {
            Self::BlockedByOwnerMigration(unresolved)
        };
    }

    /// Claims the owed restore, reporting whether it runs.
    ///
    /// A claim clears the debt in the same step that reports it, so a runtime
    /// hands its restore to exactly one caller however many sessions acquire it
    /// afterwards. The caller reports the outcome through
    /// [`SessionWorkspaceRuntime::complete_background_restore`].
    fn claim(&mut self) -> bool {
        if matches!(self, Self::Ready) {
            *self = Self::NotOwed;
            true
        } else {
            false
        }
    }

    /// The state after a claimed restore did not complete.
    ///
    /// A failure discharges nothing: the ownerless rows the pass would have
    /// restored are still unrestored, so the debt becomes payable again and a
    /// later acquisition retries it. Every blocker had cleared for the claim to
    /// be made, so no session needs recording again.
    fn reinstate(&mut self) {
        if matches!(self, Self::NotOwed) {
            *self = Self::Ready;
        }
    }
}

/// A claimed workspace restore that reinstates its debt unless it succeeds.
///
/// The claim clears the debt under one lock so a concurrent acquisition cannot
/// run the same restore twice. Dropping the claim before [`Self::confirm`] — a
/// failed pass, or a caller cancelled while the pass runs — puts the debt back
/// for a later acquisition instead of losing the obligation.
pub(in crate::zed::agent) struct OwedBackgroundRestore<'a> {
    runtime: &'a SessionWorkspaceRuntime,
    confirmed: bool,
}

impl OwedBackgroundRestore<'_> {
    /// Records that the restore ran, discharging the debt.
    pub(in crate::zed::agent) fn confirm(mut self) {
        self.runtime.complete_background_restore(true);
        self.confirmed = true;
    }
}

impl Drop for OwedBackgroundRestore<'_> {
    fn drop(&mut self) {
        if !self.confirmed {
            self.runtime.complete_background_restore(false);
        }
    }
}

impl SessionWorkspaceRuntime {
    /// Records that `session_id` has not reconciled the ownership of the rows
    /// it launched, so the workspace restore must stay blocked for it.
    pub(in crate::zed::agent) fn block_background_restore(&self, session_id: &str) {
        let mut pending = self.pending_restore_guard();
        pending.block(session_id);
    }

    /// Records that `session_id` reconciled ownership, clearing its blocker.
    ///
    /// The restore becomes payable only once every deferring session has
    /// released: a session that reconciles its own rows cannot show that
    /// another session's rows carry their owner.
    pub(in crate::zed::agent) fn release_background_restore(&self, session_id: &str) {
        let mut pending = self.pending_restore_guard();
        pending.release(session_id);
    }

    /// Claims the owed restore, leaving nothing owed until its result is known.
    ///
    /// The claim and the clear happen under one lock, so a runtime whose
    /// restore is owed hands it to exactly one caller however many sessions
    /// acquire the runtime afterwards. The returned guard must be confirmed
    /// once the restore succeeds; dropping it first reinstates the debt.
    pub(in crate::zed::agent) fn claim_owed_background_restore(&self) -> Option<OwedBackgroundRestore<'_>> {
        // The state lock is released before the guard exists. `then_some`
        // would build its argument eagerly and drop it again when nothing was
        // claimed, and that drop reinstates the debt — which takes this same
        // non-reentrant lock. The claim therefore hands the value out only
        // after the guard is gone.
        let claimed = {
            let mut pending = self.pending_restore_guard();
            pending.claim()
        };
        if claimed {
            Some(OwedBackgroundRestore { runtime: self, confirmed: false })
        } else {
            None
        }
    }

    /// Records the outcome of a claimed restore.
    ///
    /// A successful pass discharges the debt. A failed one restores it, so a
    /// later acquisition retries the restore rather than reporting work that
    /// never ran as done.
    pub(in crate::zed::agent) fn complete_background_restore(&self, restored: bool) {
        let mut pending = self.pending_restore_guard();
        if restored {
            *pending = PendingWorkspaceRestore::NotOwed;
        } else {
            pending.reinstate();
        }
    }

    /// Whether a deferred restore is still owed and held back.
    ///
    /// Test-visible: production code never needs to ask, because the claim and
    /// the release are the only decisions taken on this state.
    #[cfg(test)]
    pub(in crate::zed::agent) fn pending_background_restore_is_blocked(&self) -> bool {
        self.pending_restore_guard().is_blocked()
    }

    /// Whether a released restore is owed and waiting for a caller.
    #[cfg(test)]
    pub(in crate::zed::agent) fn pending_background_restore_is_payable(&self) -> bool {
        matches!(*self.pending_restore_guard(), PendingWorkspaceRestore::Ready)
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
        let mut state = PendingWorkspaceRestore::deferred_for("session-a");
        assert!(state.is_blocked(), "a deferred build holds the restore back");
        assert!(!state.claim(), "a blocked restore is not payable yet");

        state.release("session-a");
        assert_eq!(state, PendingWorkspaceRestore::Ready, "releasing makes the debt payable");
        assert!(state.claim(), "the first claim runs the restore and clears the debt");
        assert_eq!(state, PendingWorkspaceRestore::NotOwed, "a claim clears the debt");
        assert!(!state.claim(), "a claimed debt must not run a second time");
        state.release("session-a");
        assert_eq!(state, PendingWorkspaceRestore::NotOwed, "releasing a claimed debt must not re-arm it");
    }

    /// A runtime that restored at build time owes nothing, forever.
    ///
    /// Only a deferred build creates the debt. Releasing, or blocking for a
    /// later resume, must not arm one: ordinary `session/new` traffic would
    /// otherwise run a legacy restore the runtime never deferred.
    #[test]
    fn a_runtime_that_never_deferred_never_owes_a_restore() {
        let mut state = PendingWorkspaceRestore::NotOwed;
        state.release("session-a");
        assert_eq!(state, PendingWorkspaceRestore::NotOwed, "there is nothing to release");
        state.block("session-a");
        assert_eq!(state, PendingWorkspaceRestore::NotOwed, "a resume must not arm a restore the build already ran");
        assert!(!state.claim(), "there is nothing to claim");
    }

    /// Releasing an already-payable restore leaves it single-claim.
    ///
    /// Two acquisitions can both observe an unresolved migration before either
    /// releases it, so a second release is expected traffic and must not make
    /// the restore payable twice.
    #[test]
    fn a_second_release_does_not_make_the_restore_payable_twice() {
        let mut state = PendingWorkspaceRestore::deferred_for("session-a");
        state.release("session-a");
        state.release("session-a");
        assert_eq!(state, PendingWorkspaceRestore::Ready, "two releases leave one payable debt");

        assert!(state.claim(), "one claim pays the released debt");
        assert_eq!(state, PendingWorkspaceRestore::NotOwed);
        assert!(!state.claim(), "the second release must not create a second debt");
    }

    /// A restore that did not run leaves the debt payable for a later attempt.
    ///
    /// The claim clears the debt so a concurrent acquisition cannot run the
    /// same restore twice, but a failed pass has restored nothing: reinstating
    /// the debt is what lets the next acquisition retry instead of reporting
    /// the unrestored rows as done.
    #[test]
    fn a_failed_restore_is_payable_again() {
        let mut state = PendingWorkspaceRestore::deferred_for("session-a");
        state.release("session-a");
        assert!(state.claim(), "the released debt is claimed once");
        assert_eq!(state, PendingWorkspaceRestore::NotOwed);
        state.reinstate();
        assert_eq!(state, PendingWorkspaceRestore::Ready, "a failed pass must leave the restore payable again");
        assert!(state.claim(), "the retry claims the reinstated debt exactly as the first attempt did");

        let mut still_blocked = PendingWorkspaceRestore::deferred_for("session-a");
        still_blocked.reinstate();
        assert!(still_blocked.is_blocked(), "reinstating must not release a debt ownership still blocks");
    }

    /// One session's release cannot clear another session's blocker.
    ///
    /// A resume whose migration and retry leave rows ownerless records itself
    /// as a blocker. A later session that reconciles its own rows must not make
    /// the shared restore payable: its success says nothing about the first
    /// session's rows, which the workspace scope would relaunch as its own.
    #[test]
    fn a_restore_stays_blocked_until_every_deferring_session_reconciles() {
        let mut state = PendingWorkspaceRestore::deferred_for("session-a");
        state.block("session-b");
        state.release("session-b");
        assert_eq!(
            state,
            PendingWorkspaceRestore::blocked_by(&["session-a"]),
            "a reconciled session clears only its own blocker"
        );
        assert!(!state.claim(), "the debt stays unpayable while another session is unresolved");

        state.release("session-a");
        assert_eq!(state, PendingWorkspaceRestore::Ready, "the last reconciliation makes the debt payable");
        assert!(state.claim(), "the restore is claimable once every blocker clears");
    }

    /// A later deferred session re-blocks an already payable debt.
    #[test]
    fn a_new_blocker_holds_back_a_payable_restore() {
        let mut state = PendingWorkspaceRestore::deferred_for("session-a");
        state.release("session-a");
        assert_eq!(state, PendingWorkspaceRestore::Ready);
        state.block("session-b");
        assert_eq!(
            state,
            PendingWorkspaceRestore::blocked_by(&["session-b"]),
            "a resume that has not reconciled must hold the restore back again"
        );
        assert!(!state.claim(), "a re-blocked debt is not payable");
    }
}

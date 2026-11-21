//! Routes controller lifecycle snapshots to the adapters that registered for them.
//!
//! An observer registers against one parent session, and a child-scoped
//! controller inherits its owner's registry and observer key so grandchild
//! events still reach the adapter that registered for the root session. The
//! registration handle detaches its observer when it is dropped.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use super::{
    BackgroundSubprocessEntry, StatusEntryBuilder, SubagentController, SubagentProgressObserver, SubagentStatusEntry,
};

/// The observers registered against one controller, keyed by registration id.
#[derive(Default)]
pub(super) struct ProgressObserverRegistry {
    next_id: AtomicU64,
    observers: Mutex<BTreeMap<u64, RegisteredProgressObserver>>,
}

/// The registry a controller publishes through, the observer key it
/// publishes under, and the clock that orders its publications.
///
/// A route is snapshotted as one value so a controller joining a tree adopts
/// the registry, the clock and the key of the same observation, never a mix of
/// two.
#[derive(Clone, Default)]
pub(super) struct ProgressObserverRouteState {
    registry: Arc<ProgressObserverRegistry>,
    scope: Option<String>,
    /// Monotonic lifecycle-revision clock shared by every controller that
    /// publishes through this route.
    ///
    /// Ordering is per task, and one task's snapshots always come from the
    /// controller that owns its record, so a per-tree clock is enough: a
    /// capture takes its revision while holding the state lock, which makes
    /// revision order and capture order one total order.
    lifecycle_clock: Arc<AtomicU64>,
}

/// The swappable publication route of one controller.
///
/// Both halves move together when a controller joins another tree, so they are
/// replaced under one lock. The route is interior-mutable because attachment
/// stores the joining controller behind a shared `Arc` and can no longer take
/// `&mut` of it; without that, an independently constructed controller would
/// keep publishing into a registry nobody listens to.
#[derive(Default)]
pub(super) struct ProgressObserverRoute {
    state: Arc<std::sync::RwLock<ProgressObserverRouteState>>,
}

impl Clone for ProgressObserverRoute {
    fn clone(&self) -> Self {
        // Clones of a controller share its route, as they already share its
        // state and child-id scope: adopting a route after attachment must
        // reach every handle to the same controller.
        Self { state: Arc::clone(&self.state) }
    }
}

impl ProgressObserverRoute {
    fn snapshot(&self) -> ProgressObserverRouteState {
        self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Takes the next lifecycle revision while holding the route's read guard.
    ///
    /// The guard is what makes a reservation and a handoff mutually exclusive.
    /// Reading the clock and then incrementing it after the guard was released
    /// would let a handoff copy the value this reservation is about to take, so
    /// the tree would publish that revision twice and the adapter would drop
    /// one of the two snapshots as non-increasing.
    fn reserve(&self) -> u64 {
        let route = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        route.lifecycle_clock.fetch_add(1, Ordering::Relaxed)
    }

    /// Carries this route's clock forward into `lifecycle_clock` and adopts it.
    ///
    /// Reading this route's clock, raising the adopted clock to it and
    /// replacing the route all happen under one write guard. A reservation
    /// therefore either already happened — and is copied by `fetch_max` — or
    /// reads the adopted clock, which already covers it. Splitting the copy
    /// from the replacement would let a reservation take the copied value from
    /// the retired clock after the tree had handed the same value out again.
    pub(super) fn adopt(
        &self,
        registry: Arc<ProgressObserverRegistry>,
        scope: Option<String>,
        lifecycle_clock: Arc<AtomicU64>,
    ) {
        let mut route = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let own_revision = route.lifecycle_clock.load(Ordering::Relaxed);
        lifecycle_clock.fetch_max(own_revision, Ordering::Relaxed);
        *route = ProgressObserverRouteState { registry, scope, lifecycle_clock };
    }
}

pub(super) struct RegisteredProgressObserver {
    parent_session_id: String,
    observer: Arc<dyn SubagentProgressObserver>,
}

/// Keeps a progress observer registered with its subagent controller.
///
/// Dropping the registration detaches the observer. Adapters should retain it
/// for exactly the lifetime in which they want to receive status snapshots.
#[must_use = "dropping the registration detaches the progress observer"]
pub struct SubagentProgressObserverRegistration {
    registry: Weak<ProgressObserverRegistry>,
    id: u64,
}

impl Drop for SubagentProgressObserverRegistration {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            // Bind the removed handle and release the guard before dropping
            // it: dropping an observer may run user code, which must not
            // happen while the registry mutex is held.
            let removed = {
                let mut observers = registry.observers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                observers.remove(&self.id)
            };
            drop(removed);
        }
    }
}

impl SubagentController {
    /// Shares lifecycle observation with the controller that owns this one.
    ///
    /// The nested-delegation loop creates a child-scoped controller per child
    /// run. That controller must publish its progress through the parent's
    /// observer registry, under the key of the session that owns the run, so an
    /// adapter that registered for that session still receives grandchild
    /// snapshots.
    ///
    /// A controller can serve several host sessions at once, so the controller's
    /// own parent session is only a default: the child record names the session
    /// that actually requested the delegation, and that is the key an adapter
    /// registered against. An already-inherited scope wins over it, because
    /// every deeper run descends from the same registered session and the
    /// intermediate child's own session has no observer of its own.
    pub(crate) async fn inherit_progress_observers(&self, parent: &SubagentController, owning_session_id: &str) {
        let scope = parent.observer_scope_for(owning_session_id).await;
        self.adopt_progress_observers(parent, scope);
    }

    /// Resolves the observer key a controller publishes under for one owning
    /// session.
    ///
    /// The parent's own inherited scope wins, because every deeper run descends
    /// from the same registered session. Otherwise the session that requested
    /// the delegation is the key, and a controller with neither falls back to
    /// its configured parent session.
    pub(super) async fn observer_scope_for(&self, owning_session_id: &str) -> String {
        let route = self.progress_route.snapshot();
        match route.scope {
            Some(scope) => scope,
            None if !owning_session_id.is_empty() => owning_session_id.to_owned(),
            None => self.parent_session_id.read().await.clone(),
        }
    }

    /// Publishes this controller's snapshots through `parent`'s registry under
    /// the key of the route it currently publishes through.
    ///
    /// This is split from [`Self::inherit_progress_observers`] so a caller that
    /// has already resolved the key can adopt the route without awaiting while
    /// it holds a controller state lock.
    pub(super) fn adopt_progress_observers(&self, parent: &SubagentController, fallback_scope: String) {
        let parent_route = parent.progress_route.snapshot();
        self.adopt_progress_route(&parent_route, fallback_scope.as_str());
    }

    /// Snapshots the route this controller publishes through.
    ///
    /// A caller that adopts a route on behalf of several controllers takes one
    /// snapshot and hands it to each of them, so every controller of a grafted
    /// subtree publishes through the same registry, clock and key.
    pub(super) fn progress_route_snapshot(&self) -> ProgressObserverRouteState {
        self.progress_route.snapshot()
    }

    /// Publishes this controller's snapshots through `route`.
    ///
    /// The route's own key wins over `fallback_scope`: a controller that has
    /// already joined a tree publishes under that tree's session, and a
    /// caller's earlier resolution may predate the join. Only a route with no
    /// inherited key falls back, and taking the registry, the clock and the key
    /// from one snapshot keeps all three consistent.
    pub(super) fn adopt_progress_route(&self, route: &ProgressObserverRouteState, fallback_scope: &str) {
        let scope = match route.scope.clone() {
            Some(scope) => scope,
            None => fallback_scope.to_owned(),
        };
        self.progress_route
            .adopt(Arc::clone(&route.registry), Some(scope), Arc::clone(&route.lifecycle_clock));
    }

    /// Allocates the next lifecycle revision for this controller tree.
    ///
    /// Publication paths call this while they hold the state lock that read
    /// the record they are about to publish, so the revision is a
    /// linearization index of the captured state: a capture taken before a
    /// later state change always carries the lower revision, whichever order
    /// the two are delivered in. The ACP adapter reserves the initial
    /// snapshot pass's base revision here too, before it registers its
    /// observer, so every later capture outranks the snapshots.
    #[must_use]
    pub fn reserve_lifecycle_revision(&self) -> u64 {
        self.progress_route.reserve()
    }

    /// Registers an observer for lifecycle snapshots belonging to one parent
    /// session. Separate registrations allow multiple adapters or sessions to
    /// observe the same controller without replacing one another.
    pub fn register_progress_observer(
        &self,
        parent_session_id: impl Into<String>,
        observer: Arc<dyn SubagentProgressObserver>,
    ) -> SubagentProgressObserverRegistration {
        // Convert before taking the lock: `Into<String>` is caller-defined and
        // may run code that re-enters this registry, which would deadlock
        // against the guard held here.
        let parent_session_id = parent_session_id.into();
        let registry = self.progress_route.snapshot().registry;
        let id = registry.next_id.fetch_add(1, Ordering::Relaxed);
        let mut observers = registry.observers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        observers.insert(id, RegisteredProgressObserver { parent_session_id, observer });
        SubagentProgressObserverRegistration { registry: Arc::downgrade(&registry), id }
    }

    fn with_progress_observers(&self, parent_session_id: &str, notify: impl Fn(&dyn SubagentProgressObserver)) {
        // A child-scoped controller inherits its owner's observer key, so
        // grandchild events reach the adapter registered for the root session
        // instead of being dropped for an unknown intermediate session.
        let route = self.progress_route.snapshot();
        let observer_key = route.scope.as_deref().unwrap_or(parent_session_id);
        let observers = {
            let observers = route
                .registry
                .observers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            observers
                .values()
                .filter(|registration| registration.parent_session_id == observer_key)
                .map(|registration| Arc::clone(&registration.observer))
                .collect::<Vec<_>>()
        };
        for observer in observers {
            notify(observer.as_ref());
        }
    }

    pub(crate) async fn publish_subagent_progress(&self, task: SubagentStatusEntry) {
        // Publish the record as it is now, not the caller's capture. Callers
        // build their entry under their own lock and only later await this
        // call, so a newer state can be recorded and published in that gap.
        // Stamping the older capture here would give it the newer revision and
        // let it overwrite the newer state; re-reading under the stamping lock
        // keeps revision order and capture order the same total order.
        let Some((entry, lifecycle_revision)) = self.observed_child_status(&task.id).await else {
            // Child records are never removed from the state, so a missing id
            // is not reachable in-tree. Dropping is safer than publishing a
            // snapshot whose ordering evidence is unknown.
            return;
        };
        self.notify_child_observers(entry, lifecycle_revision);
    }

    /// Notifies observers registered for a child snapshot's parent session.
    fn notify_child_observers(&self, entry: SubagentStatusEntry, lifecycle_revision: u64) {
        self.with_progress_observers(&entry.parent_thread_id, |observer| {
            observer.subagent_status_published(entry.clone(), lifecycle_revision);
        });
    }

    /// Reads one child's status projection under the state lock and stamps it
    /// with the next lifecycle revision.
    ///
    /// The stamp is taken while the lock that read the record is held, so a
    /// capture taken before a later state change can only carry a lower
    /// revision than one taken after it.
    async fn observed_child_status(&self, child_id: &str) -> Option<(SubagentStatusEntry, u64)> {
        let state = self.state.read().await;
        let entry = state.children.get(child_id)?.build_status_entry();
        let lifecycle_revision = self.reserve_lifecycle_revision();
        Some((entry, lifecycle_revision))
    }

    /// Reads one background record's parent session and status projection
    /// under a single state lock, and stamps the projection with the next
    /// lifecycle revision.
    ///
    /// Publishing needs the projection, the parent session, and the revision
    /// together. Reading them under separate acquisitions could pair an entry
    /// observed before an update with a parent session observed after a later
    /// one, so a status list would mix two observations.
    async fn observed_background_status(&self, record_id: &str) -> Option<(String, BackgroundSubprocessEntry, u64)> {
        let state = self.state.read().await;
        let record = state.background_children.get(record_id)?;
        let entry = record.build_status_entry();
        let lifecycle_revision = self.reserve_lifecycle_revision();
        Some((record.parent_session_id.clone(), entry, lifecycle_revision))
    }

    /// Notifies observers registered for one parent session of a background
    /// status snapshot.
    pub(super) fn notify_background_observers(
        &self,
        parent_session_id: &str,
        task: BackgroundSubprocessEntry,
        lifecycle_revision: u64,
    ) {
        if parent_session_id.is_empty() {
            return;
        }
        self.with_progress_observers(parent_session_id, |observer| {
            observer.background_status_published(task.clone(), lifecycle_revision);
        });
    }

    /// Publishes the current projection of one background record and returns
    /// it.
    ///
    /// The `parent_session_id` that routes the snapshot and the snapshot itself
    /// come from one state read, so a restart that moves the record to another
    /// parent between two reads cannot route an older projection to the new
    /// parent's observers. See `settle_background_status_and_publish` for the
    /// parent-session override rule.
    pub(crate) async fn publish_background_progress(&self, record_id: &str) -> Option<BackgroundSubprocessEntry> {
        let (parent_session_id, entry, lifecycle_revision) = self.observed_background_status(record_id).await?;
        self.notify_background_observers(&parent_session_id, entry.clone(), lifecycle_revision);
        Some(entry)
    }

    pub(crate) async fn publish_child_status(&self, child_id: &str) {
        if let Some((entry, lifecycle_revision)) = self.observed_child_status(child_id).await {
            self.notify_child_observers(entry, lifecycle_revision);
        }
    }

    pub(crate) async fn publish_background_status(&self, record_id: &str) {
        if let Some((parent_session_id, entry, lifecycle_revision)) = self.observed_background_status(record_id).await {
            self.notify_background_observers(&parent_session_id, entry, lifecycle_revision);
        }
    }
}

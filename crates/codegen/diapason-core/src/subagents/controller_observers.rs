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
    BackgroundRecord, BackgroundSubprocessEntry, ChildRecord, StatusEntryBuilder, SubagentController,
    SubagentProgressObserver, SubagentStatusEntry,
};

/// The observers registered against one controller, keyed by registration id.
#[derive(Default)]
pub(super) struct ProgressObserverRegistry {
    next_id: AtomicU64,
    observers: Mutex<BTreeMap<u64, RegisteredProgressObserver>>,
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
            let mut observers = registry.observers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            observers.remove(&self.id);
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
    pub(crate) async fn inherit_progress_observers(&mut self, parent: &SubagentController, owning_session_id: &str) {
        self.progress_observers = Arc::clone(&parent.progress_observers);
        self.progress_observer_scope = Some(match parent.progress_observer_scope.clone() {
            Some(scope) => scope,
            None if !owning_session_id.is_empty() => owning_session_id.to_owned(),
            None => parent.parent_session_id.read().await.clone(),
        });
    }

    /// Registers an observer for lifecycle snapshots belonging to one parent
    /// session. Separate registrations allow multiple adapters or sessions to
    /// observe the same controller without replacing one another.
    pub fn register_progress_observer(
        &self,
        parent_session_id: impl Into<String>,
        observer: Arc<dyn SubagentProgressObserver>,
    ) -> SubagentProgressObserverRegistration {
        let id = self.progress_observers.next_id.fetch_add(1, Ordering::Relaxed);
        let mut observers = self
            .progress_observers
            .observers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        observers.insert(
            id,
            RegisteredProgressObserver {
                parent_session_id: parent_session_id.into(),
                observer,
            },
        );
        SubagentProgressObserverRegistration {
            registry: Arc::downgrade(&self.progress_observers),
            id,
        }
    }

    fn with_progress_observers(&self, parent_session_id: &str, notify: impl Fn(&dyn SubagentProgressObserver)) {
        // A child-scoped controller inherits its owner's observer key, so
        // grandchild events reach the adapter registered for the root session
        // instead of being dropped for an unknown intermediate session.
        let observer_key = self.progress_observer_scope.as_deref().unwrap_or(parent_session_id);
        let observers = {
            let observers = self
                .progress_observers
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
        self.with_progress_observers(&task.parent_thread_id, |observer| observer.subagent_status_changed(task.clone()));
    }

    pub(crate) async fn publish_background_progress(&self, task: BackgroundSubprocessEntry) {
        let parent_session_id = {
            let state = self.state.read().await;
            state
                .background_children
                .get(&task.id)
                .map(|record| record.parent_session_id.clone())
        };
        if let Some(parent_session_id) = parent_session_id.filter(|owner| !owner.is_empty()) {
            self.with_progress_observers(&parent_session_id, |observer| {
                observer.background_status_changed(task.clone())
            });
        }
    }

    pub(crate) async fn publish_child_status(&self, child_id: &str) {
        let entry = {
            let state = self.state.read().await;
            state.children.get(child_id).map(ChildRecord::build_status_entry)
        };
        if let Some(entry) = entry {
            self.publish_subagent_progress(entry).await;
        }
    }

    pub(crate) async fn publish_background_status(&self, record_id: &str) {
        let entry = {
            let state = self.state.read().await;
            state
                .background_children
                .get(record_id)
                .map(BackgroundRecord::build_status_entry)
        };
        if let Some(entry) = entry {
            self.publish_background_progress(entry).await;
        }
    }
}

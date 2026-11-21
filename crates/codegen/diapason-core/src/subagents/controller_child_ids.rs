//! Child task ids allocated across one controller tree, and the graft that joins a tree.
//!
//! A controller only holds its own children, so checking that map alone cannot
//! see an id a sibling controller already allocated. The owning session
//! exposes the whole tree at once and routes a task id to the first controller
//! that matches, so a duplicate id would list one task twice and send an
//! output read or a cancel to whichever controller happened to come first.
//! The root controller owns one scope and the whole tree shares it through one
//! [`ChildIdScopeSlot`]; ids are recorded for the lifetime of the tree.
use std::collections::BTreeSet;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use super::controller_locks::{SubagentLockScope, default_lock_scope};

/// A test-only rendezvous around the reservation window of [`ChildIdScopeSlot::with_scope`].
///
/// Production never installs one. A test that must prove the adoption lock
/// covers a reservation parks the first allocation between resolving its
/// terminal scope and recording its id, while the allocation still holds the
/// adoption lock. The graft then signals that it has reached that lock and must
/// block there. Releasing the gate lets the reservation be recorded first, so
/// the graft's later merge is guaranteed to see the id.
#[cfg(test)]
#[derive(Default)]
pub(super) struct AllocationRendezvous {
    parked_once: AtomicBool,
    entered: tokio::sync::Notify,
    merge_attempted: tokio::sync::Notify,
    adoption_lock_held: tokio::sync::Notify,
    reserved: AtomicBool,
    released: Mutex<bool>,
    release: std::sync::Condvar,
}

#[cfg(test)]
impl AllocationRendezvous {
    /// Parks the first allocation that reaches the gate until [`Self::release`].
    ///
    /// Returns whether this call parked. Later allocations pass straight
    /// through, so a tree's remaining fixtures are unaffected once the gated
    /// reservation is complete.
    fn park_once(&self) -> bool {
        if self.parked_once.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.entered.notify_one();
        let released = self.released.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // The wait is bounded so a failing test reports its assertion instead
        // of hanging a parked worker when the release never arrives.
        let _ = self
            .release
            .wait_timeout_while(released, std::time::Duration::from_secs(5), |released| !*released)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        true
    }

    /// Waits until the gated allocation has parked in the reservation window.
    pub(super) async fn wait_until_parked(&self) {
        self.entered.notified().await;
    }

    /// Records that the gated allocation reserved its id.
    fn mark_reserved(&self) {
        self.reserved.store(true, Ordering::SeqCst);
    }

    /// Returns whether the gated allocation reserved its id before the merge.
    pub(super) fn reserved_before_merge(&self) -> bool {
        self.reserved.load(Ordering::SeqCst)
    }

    /// Signals that a graft has reached the adoption lock it must acquire.
    fn signal_merge_attempt(&self) {
        self.merge_attempted.notify_one();
    }

    /// Waits until a graft has reached the adoption lock.
    pub(super) async fn wait_for_merge_attempt(&self) {
        self.merge_attempted.notified().await;
    }

    /// Signals that a graft now holds the adoption lock.
    fn signal_adoption_lock_held(&self) {
        self.adoption_lock_held.notify_one();
    }

    /// Waits until a graft has acquired the adoption lock.
    pub(super) async fn wait_for_adoption_lock_held(&self) {
        self.adoption_lock_held.notified().await;
    }

    /// Releases a parked allocation so it can record its id.
    pub(super) fn release(&self) {
        *self.released.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        self.release.notify_all();
    }
}

/// The child task ids allocated anywhere in one controller tree.
///
/// A scope is either a live allocator or a forwarding link to the allocator of
/// the tree its subtree joined. Grafting does not rewrite the slot of every
/// controller in the adopted subtree: a controller attached earlier holds a
/// slot that names this scope, not the slot the graft redirects. Recording the
/// target here instead lets those slots follow the link when they next
/// allocate, so the whole adopted chain reaches the tree's live allocator.
#[derive(Default)]
pub(super) struct ChildIdScope {
    taken: Mutex<BTreeSet<String>>,
    /// The live allocator this scope forwards to, once its subtree was grafted.
    ///
    /// `None` marks a terminal scope, which is the one accepting ids. The chain
    /// cannot cycle: [`ChildIdScopeSlot::adopt_into`] resolves both sides to
    /// their terminals under the tree's adoption lock, so an edge it
    /// writes always points at a scope that had no redirect at the time it was
    /// written, and no redirect is ever written out of a node that already has
    /// one.
    redirect: Mutex<Option<Arc<ChildIdScope>>>,
}

impl ChildIdScope {
    /// Follows the forwarding links to the scope that owns the allocator.
    ///
    /// Every link was written under the adoption lock, so a caller that holds
    /// that lock sees a settled chain. Links never cycle, so the walk
    /// terminates at the tree's live allocator.
    fn terminal(scope: &Arc<Self>) -> Arc<Self> {
        let mut current = Arc::clone(scope);
        loop {
            let next = current.redirect.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            match next {
                Some(next) => current = next,
                None => return current,
            }
        }
    }

    /// Allocates `candidate` or its first free suffix, and records the result.
    ///
    /// The caller supplies the controller-local collision check; this scope
    /// adds the tree-wide one and remembers the allocated id so a later
    /// allocation in any controller of the tree cannot repeat it.
    pub(super) fn allocate(&self, is_taken: impl Fn(&str) -> bool, candidate: &str) -> String {
        let mut taken = self.taken.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let id = unique_child_id(|id| taken.contains(id) || is_taken(id), candidate);
        taken.insert(id.clone());
        id
    }
}

/// The redirectable allocator pointer every controller in one subtree shares.
///
/// A child controller adopts its parent's slot object rather than a copy of
/// the scope inside it, so the slot is shared by the controller that owns it
/// and by every descendant it has already created. Writing a scope into the
/// slot therefore redirects that whole existing subtree at once, which is what
/// attaching an independently created controller that already has children
/// must do; redirecting only the attached controller would leave its
/// descendants allocating from the stale scope, free to repeat an id the tree
/// has already issued. A descendant that was attached before the graft points
/// at the adopted subtree's former scope instead of this slot, and reaches the
/// tree through that scope's forwarding link.
pub(super) struct ChildIdScopeSlot {
    scope: RwLock<Arc<ChildIdScope>>,
    /// The tree's adoption lock, shared with every controller that adopts this
    /// slot. Holding it for the whole resolution and allocation keeps an
    /// adoption from redirecting the chain out from under a reserved id.
    ///
    /// The controller keeps its own lock order around this module rather than
    /// the reverse: attaching takes the target controller's state write lock
    /// before `adopt_into`, and inserting a child takes it before `with_scope`.
    /// Nothing here takes a controller state lock, so the two orders cannot
    /// cross.
    lock_scope: Arc<SubagentLockScope>,
    /// The rendezvous a test installed to observe the reservation window.
    ///
    /// Production slots never carry one; `cfg(test)` keeps the extra field out
    /// of every non-test build.
    #[cfg(test)]
    allocation_rendezvous: Mutex<Option<Arc<AllocationRendezvous>>>,
}

/// A default slot binds to the process-global locks, not to a fresh scope.
///
/// Deriving `Default` would give every default slot its own `Arc` and silently
/// drop the cross-controller exclusion the former static lock provided.
impl Default for ChildIdScopeSlot {
    fn default() -> Self {
        Self::with_lock_scope(default_lock_scope())
    }
}

impl ChildIdScopeSlot {
    /// Builds a slot bound to the tree's adoption lock.
    pub(super) fn with_lock_scope(lock_scope: Arc<SubagentLockScope>) -> Self {
        Self {
            scope: RwLock::new(Arc::default()),
            lock_scope,
            #[cfg(test)]
            allocation_rendezvous: Mutex::new(None),
        }
    }

    /// Installs a test-only rendezvous on this slot.
    ///
    /// The same gate must also be installed on the slot whose graft will merge
    /// this allocation, so [`Self::adopt_into`] can report reaching the lock.
    #[cfg(test)]
    pub(super) fn install_allocation_rendezvous(&self, rendezvous: Arc<AllocationRendezvous>) {
        *self
            .allocation_rendezvous
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(rendezvous);
    }

    /// Runs `allocate` against this subtree's live allocator.
    ///
    /// The slot resolves through any forwarding links first, so a controller
    /// grafted beneath another tree allocates from the tree it joined. The
    /// adoption lock is held for the whole resolution and allocation, so an
    /// adoption cannot redirect the chain out from under an id being reserved.
    pub(super) fn with_scope<R>(&self, allocate: impl FnOnce(&ChildIdScope) -> R) -> R {
        let _exclusive = self
            .lock_scope
            .adoption()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let scope = self.scope.read().unwrap_or_else(|poisoned| poisoned.into_inner());
        let terminal = ChildIdScope::terminal(&scope);
        // Tests that need to observe the reservation window park the first
        // allocation here, while the adoption lock above is still held. The
        // id is recorded before the lock is released, so a graft cannot merge
        // in between.
        #[cfg(test)]
        let rendezvous = self
            .allocation_rendezvous
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        #[cfg(test)]
        let parked = rendezvous.as_ref().is_some_and(|rendezvous| rendezvous.park_once());
        let allocated = allocate(&terminal);
        #[cfg(test)]
        if let Some(rendezvous) = &rendezvous
            && parked
        {
            rendezvous.mark_reserved();
        }
        allocated
    }

    /// Moves this whole subtree onto `target`'s allocator.
    ///
    /// Both slots are resolved to the live allocator of their tree first, so
    /// attaching a controller that was itself grafted earlier still joins the
    /// tree it now belongs to. If both sides are already the same allocator
    /// there is nothing to do: joining a controller to the tree it already
    /// belongs to is the common case, because a child inherits its parent's
    /// slot before it is attached.
    ///
    /// Returns the id both allocators already hold, if there is one, without
    /// changing either side. An id that both sides have already published
    /// cannot be separated by a suffix, and silently merging the sets would
    /// leave the adapter listing one task twice and routing an output read or
    /// a cancel to whichever controller it reached first.
    ///
    /// The merge and the redirect run under the tree's adoption lock, so no id
    /// can be reserved anywhere in that tree while the two allocators are
    /// reconciled. Both slots must therefore bind the same lock: an adoption
    /// that held only the joining subtree's lock would not exclude an
    /// allocation in the tree it is joining, and an id could be reserved in
    /// the allocator the graft is about to redirect. Slots bound to different
    /// locks are rejected rather than merged.
    ///
    /// The ids this subtree holds are exactly the ones the target now re-uses
    /// through the forwarding link, and because the target is terminal when
    /// the link is written, the chain stays acyclic.
    pub(super) fn adopt_into(&self, target: &Self) -> Result<(), AdoptionRejection> {
        if !Arc::ptr_eq(&self.lock_scope, &target.lock_scope) {
            return Err(AdoptionRejection::LockScopeMismatch);
        }
        // A test that installed a rendezvous learns that the merge has reached
        // the adoption lock; it can then release a parked reservation and know
        // the merge cannot have happened first.
        #[cfg(test)]
        if let Some(rendezvous) = self
            .allocation_rendezvous
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            rendezvous.signal_merge_attempt();
        }
        let _exclusive = self
            .lock_scope
            .adoption()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A test that installed a rendezvous learns that the merge now holds
        // the lock, so it can tell an acquisition from a mere attempt.
        #[cfg(test)]
        if let Some(rendezvous) = self
            .allocation_rendezvous
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            rendezvous.signal_adoption_lock_held();
        }
        let source = {
            let slot = self.scope.read().unwrap_or_else(|poisoned| poisoned.into_inner());
            ChildIdScope::terminal(&slot)
        };
        let target_scope = {
            let slot = target.scope.read().unwrap_or_else(|poisoned| poisoned.into_inner());
            ChildIdScope::terminal(&slot)
        };
        if Arc::ptr_eq(&source, &target_scope) {
            return Ok(());
        }
        // The scopes cannot have changed since they were resolved: only an
        // adoption writes a redirect, the tree's adoption lock above excludes
        // every other one in that tree, and allocation holds it too.
        let mut slot = self.scope.write().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut source_taken = source.taken.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut target_taken = target_scope.taken.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(duplicate) = source_taken.intersection(&target_taken).next() {
            return Err(AdoptionRejection::Duplicate(duplicate.clone()));
        }
        // The reservations move rather than copy: once the redirect below is
        // written the source allocator forwards here, so leaving a copy behind
        // would keep two id spaces claiming the same ids. Nothing reads the
        // source set after this point; its terminal link is what a later
        // allocation follows.
        target_taken.extend(std::mem::take(&mut *source_taken));
        drop(target_taken);
        drop(source_taken);
        *source.redirect.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::clone(&target_scope));
        *slot = Arc::clone(&target_scope);
        Ok(())
    }
}

/// Why a subtree could not join a tree's allocator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AdoptionRejection {
    /// Both allocators have already published the same id.
    Duplicate(String),
    /// The two slots do not share one adoption lock, so the merge could not be
    /// made exclusive against the tree being joined.
    LockScopeMismatch,
}

impl std::fmt::Display for AdoptionRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Duplicate(duplicate) => {
                write!(formatter, "task id {duplicate} is already allocated on both sides")
            }
            Self::LockScopeMismatch => formatter.write_str("the controllers do not share one adoption lock scope"),
        }
    }
}

/// Returns `candidate`, or the first `-<n>` suffix of it that `is_taken` does
/// not already hold.
///
/// A generated id is `<kind>-<millisecond timestamp>`, so the only collision
/// it can suffer is a second spawn of the same kind in the same millisecond.
/// Suffixing keeps both records addressable instead of letting the second
/// insert replace the first. Ids reach this function only from the generator
/// in `prepare_queued_child`, so no caller-supplied naming scheme gains an
/// unexpected suffix.
fn unique_child_id(is_taken: impl Fn(&str) -> bool, candidate: &str) -> String {
    if !is_taken(candidate) {
        return candidate.to_owned();
    }
    let mut suffix = 2_u32;
    loop {
        let id = format!("{candidate}-{suffix}");
        if !is_taken(&id) {
            return id;
        }
        suffix = suffix.saturating_add(1);
    }
}

#[cfg(test)]
mod tests;

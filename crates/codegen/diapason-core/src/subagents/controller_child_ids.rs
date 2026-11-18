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
use std::sync::{Arc, Mutex, RwLock};

use super::controller_locks::{SubagentLockScope, default_lock_scope};

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
        Self { scope: RwLock::new(Arc::default()), lock_scope }
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
        allocate(&terminal)
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
        let _exclusive = self
            .lock_scope
            .adoption()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let source_taken = source.taken.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut target_taken = target_scope.taken.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(duplicate) = source_taken.intersection(&target_taken).next() {
            return Err(AdoptionRejection::Duplicate(duplicate.clone()));
        }
        target_taken.extend(source_taken.iter().cloned());
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
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

    /// Resolves which slot indices a sequence of grafts joined.
    ///
    /// The test needs the resulting trees, not the graft order, so a small
    /// union-find models the joins the production code applied.
    fn joined_components(count: usize, grafts: &[(usize, usize)]) -> Vec<usize> {
        fn root(parent: &mut [usize], mut index: usize) -> usize {
            while parent[index] != index {
                index = parent[index];
            }
            index
        }
        let mut parent: Vec<usize> = (0..count).collect();
        for &(left, right) in grafts {
            let (left, right) = (root(&mut parent, left), root(&mut parent, right));
            if left != right {
                parent[left] = right;
            }
        }
        (0..count).map(|index| root(&mut parent, index)).collect()
    }

    proptest! {
        /// A grafted tree allocates from one id space.
        ///
        /// Each controller starts with its own allocator and a namespace of ids
        /// that cannot collide, so every graft succeeds. After a random sequence
        /// of grafts, every slot in one tree must draw from the same allocator:
        /// the same candidate proposed from all of them has to come back
        /// suffixed rather than repeated. A descendant that kept its old
        /// allocator would return the bare candidate too, and the tree would
        /// hold one id twice.
        #[test]
        fn grafting_joins_every_descendant_onto_one_id_space(
            existing in prop::collection::vec(0usize..3, 1..=5),
            raw_grafts in prop::collection::vec((0usize..5, 0usize..5), 0..=8),
            candidates in prop::collection::vec("[a-z]{1,8}", 1..=4),
        ) {
            let slots = existing.iter().map(|_| ChildIdScopeSlot::default()).collect::<Vec<_>>();
            for (index, count) in existing.iter().enumerate() {
                for ordinal in 0..*count {
                    let candidate = format!("pre-{index}-{ordinal}");
                    let allocated = slots[index].with_scope(|scope| scope.allocate(|_| false, &candidate));
                    prop_assert_eq!(allocated, candidate, "a namespaced id must be issued verbatim");
                }
            }

            let mut applied: Vec<(usize, usize)> = Vec::new();
            for &(start, end) in &raw_grafts {
                let start = start % slots.len();
                let end = end % slots.len();
                if start == end {
                    continue;
                }
                slots[start].adopt_into(&slots[end]).map_err(|rejection| {
                    TestCaseError::fail(format!("every slot binds the default scope, so only a duplicate can reject: {rejection}"))
                })?;
                applied.push((start, end));
            }

            let component = joined_components(slots.len(), &applied);
            for candidate in &candidates {
                let mut issued: BTreeMap<usize, Vec<String>> = BTreeMap::new();
                for (index, slot) in slots.iter().enumerate() {
                    let allocated = slot.with_scope(|scope| scope.allocate(|_| false, candidate));
                    issued.entry(component[index]).or_default().push(allocated);
                }
                for (tree, ids) in issued {
                    let unique = ids.iter().collect::<BTreeSet<_>>();
                    prop_assert_eq!(
                        unique.len(),
                        ids.len(),
                        "tree {} issued {} twice: {:?}",
                        tree,
                        candidate,
                        ids,
                    );
                }
            }
        }
    }

    /// Generated ids carry millisecond precision, so two spawns of the same
    /// kind in one millisecond propose the same id. The second must receive a
    /// distinct id: `BTreeMap::insert` on the shared key would replace the
    /// first child's record and silently lose a subagent that had already been
    /// admitted.
    #[test]
    fn a_colliding_child_id_is_suffixed_instead_of_replacing_the_record() {
        let mut taken: BTreeSet<String> = BTreeSet::new();

        let first = unique_child_id(|id| taken.contains(id), "agent-explorer-20261113T060819731Z");
        taken.insert(first.clone());
        let second = unique_child_id(|id| taken.contains(id), "agent-explorer-20261113T060819731Z");
        taken.insert(second.clone());
        let third = unique_child_id(|id| taken.contains(id), "agent-explorer-20261113T060819731Z");

        assert_eq!(
            [first.as_str(), second.as_str(), third.as_str()],
            [
                "agent-explorer-20261113T060819731Z",
                "agent-explorer-20261113T060819731Z-2",
                "agent-explorer-20261113T060819731Z-3",
            ],
            "each collision must take the next free suffix"
        );
        assert!(!taken.contains(&third), "the third id must still be free");
    }

    /// Allocation and adoption exclude each other through the tree lock.
    ///
    /// A graft reconciles the two allocators and only then rewrites the
    /// subtree's slot. An allocation that resolved the old allocator in between
    /// would record its id where the tree no longer reads it, and the tree would
    /// issue that id again. Both sides therefore take one lock for the whole
    /// step: the allocation for its resolve-and-record, the graft for its
    /// merge-and-redirect. A concurrent run cannot show that reliably — the
    /// window is microseconds wide — so this holds the lock itself and asserts
    /// that neither step can finish while it is held.
    #[test]
    fn allocation_and_adoption_wait_for_the_tree_adoption_lock() {
        let lock_scope = Arc::new(SubagentLockScope::new());
        let source = Arc::new(ChildIdScopeSlot::with_lock_scope(Arc::clone(&lock_scope)));
        let target = Arc::new(ChildIdScopeSlot::with_lock_scope(Arc::clone(&lock_scope)));
        let candidate = "blocked-candidate";
        let wait = Duration::from_millis(250);

        let guard = lock_scope.adoption().lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let (allocated_tx, allocated_rx) = std::sync::mpsc::channel();
        let allocating_source = Arc::clone(&source);
        let allocating = std::thread::spawn(move || {
            let id = allocating_source.with_scope(|scope| scope.allocate(|_| false, candidate));
            let _ = allocated_tx.send(id);
        });
        assert!(allocated_rx.recv_timeout(wait).is_err(), "an allocation must wait for the tree adoption lock");

        let (adopted_tx, adopted_rx) = std::sync::mpsc::channel();
        let adopting_source = Arc::clone(&source);
        let adopting_target = Arc::clone(&target);
        let adopting = std::thread::spawn(move || {
            let outcome = adopting_source.adopt_into(&adopting_target);
            let _ = adopted_tx.send(outcome.is_ok());
        });
        assert!(adopted_rx.recv_timeout(wait).is_err(), "a graft must wait for the tree adoption lock");

        drop(guard);

        assert_eq!(
            allocated_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the allocation resumes"),
            candidate,
            "the waiting allocation must still take the free candidate"
        );
        assert!(
            adopted_rx.recv_timeout(Duration::from_secs(5)).expect("the graft resumes"),
            "the waiting graft must still succeed"
        );
        allocating.join().expect("the allocating thread finishes");
        adopting.join().expect("the adopting thread finishes");

        // Whichever step the lock admitted first, the id it recorded is the one
        // the grafted tree now reads, so the tree can never issue it again.
        let repeated = target.with_scope(|scope| scope.allocate(|_| false, candidate));
        assert_ne!(repeated, candidate, "the tree must refuse an id recorded on either side of the graft");
    }
}

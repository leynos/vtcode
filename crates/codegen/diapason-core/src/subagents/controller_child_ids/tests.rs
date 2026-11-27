//! Pins the child-id allocation and subtree-graft contracts.
//!
//! A graft must leave every controller in the joined tree drawing from one id
//! space, a colliding id must never replace an admitted record, and allocation
//! and adoption must exclude each other through the tree's adoption lock.
use super::*;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
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

        // A reservation made before the graft must move into the joined id
        // space, not merely stay behind in the allocating subtree. Proposing
        // the same id again from a *different* slot of the same tree must
        // therefore come back suffixed: a graft that never transferred the
        // reservation would hand the bare id out a second time.
        for (index, count) in existing.iter().enumerate() {
            for ordinal in 0..*count {
                let reserved = format!("pre-{index}-{ordinal}");
                let peer = (0..slots.len())
                    .find(|peer| *peer != index && component[*peer] == component[index]);
                let Some(peer) = peer else {
                    // A singleton component has no other slot to ask, so the
                    // transfer is unobservable there.
                    continue;
                };
                let reissued = slots[peer].with_scope(|scope| scope.allocate(|_| false, &reserved));
                prop_assert_eq!(
                    reissued,
                    format!("{reserved}-2"),
                    "tree {} must reserve id {} allocated by another of its slots",
                    component[index],
                    reserved,
                );
            }
        }

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

/// Disjoint trees each keep their own id space.
///
/// A task id is only unique within the controller tree that routes it: two
/// sessions never resolve each other's tasks, so the same generated id in two
/// independent trees addresses one task in each. The graft regression above
/// pins the per-tree check; this pins its complement, so a future change
/// cannot make allocation reject a candidate another tree happens to hold.
#[test]
fn disjoint_trees_can_issue_the_same_base_id() {
    let left = ChildIdScopeSlot::default();
    let right = ChildIdScopeSlot::default();
    let candidate = "agent-explorer-20261113T060819731Z";

    let left_id = left.with_scope(|scope| scope.allocate(|_| false, candidate));
    let right_id = right.with_scope(|scope| scope.allocate(|_| false, candidate));

    assert_eq!(left_id, candidate, "an empty tree issues the bare candidate");
    assert_eq!(right_id, candidate, "a disjoint tree must not reserve an id another tree issued");
}

/// A slot bound to another adoption lock is never adopted.
///
/// The merge and redirect run under the tree's adoption lock, so a slot bound
/// to a different lock could not be made exclusive against an allocation in the
/// tree being joined. The attachment reports that mismatch itself, before it
/// inspects the joining subtree; this pins the allocator's own check, which
/// remains the last line of defence for a caller that reaches it directly.
#[test]
fn a_slot_bound_to_another_adoption_lock_is_not_adopted() {
    let source = ChildIdScopeSlot::with_lock_scope(Arc::new(SubagentLockScope::new()));
    let target = ChildIdScopeSlot::with_lock_scope(Arc::new(SubagentLockScope::new()));
    let candidate = "agent-explorer-20261113T060819731Z";

    assert_eq!(
        source.with_scope(|scope| scope.allocate(|_| false, candidate)),
        candidate,
        "the source reserves the bare candidate"
    );

    let rejection = source
        .adopt_into(&target)
        .expect_err("a slot bound to another adoption lock must not join this tree");
    assert!(
        matches!(rejection, AdoptionRejection::LockScopeMismatch),
        "the rejection must name the scope mismatch: {rejection}"
    );

    // The rejected merge leaves both allocators as they were: the target never
    // gains the source's reservation, and the source still holds it.
    assert_eq!(
        target.with_scope(|scope| scope.allocate(|_| false, candidate)),
        candidate,
        "a rejected merge must not carry the source's reservation into the target"
    );
    assert_eq!(
        source.with_scope(|scope| scope.allocate(|_| false, candidate)),
        format!("{candidate}-2"),
        "a rejected merge must leave the source's allocator intact"
    );
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

/// Holds the tree adoption lock on a blocking thread until the sender drops.
///
/// The test awaits the workers while the lock must stay held, and a standard
/// library guard must not live across an await point, so the guard owns a
/// thread of its own. The call returns only once the guard is held, so a
/// worker spawned afterwards cannot acquire the lock behind the test's back.
fn hold_adoption_lock(
    lock_scope: &Arc<SubagentLockScope>,
) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let lock_scope = Arc::clone(lock_scope);
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = lock_scope.adoption().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = held_tx.send(());
        let _ = release_rx.recv();
    });
    held_rx.recv().expect("the holding thread acquires the tree adoption lock");
    (release_tx, holder)
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
///
/// Each step reports from inside the lock rather than before it. While the
/// guard is held, neither report may arrive: the allocation parks in its
/// reservation window inside `with_scope` only after acquiring the lock, and
/// the graft reports that it holds the lock only after acquiring it. A report
/// arriving early therefore means that step never took the lock. Releasing the
/// guard must then produce both reports, which is the positive evidence that
/// each step actually acquired the lock instead of merely finishing late.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn allocation_and_adoption_wait_for_the_tree_adoption_lock() {
    let lock_scope = Arc::new(SubagentLockScope::new());
    let source = Arc::new(ChildIdScopeSlot::with_lock_scope(Arc::clone(&lock_scope)));
    let target = Arc::new(ChildIdScopeSlot::with_lock_scope(Arc::clone(&lock_scope)));
    let rendezvous = Arc::new(AllocationRendezvous::default());
    source.install_allocation_rendezvous(Arc::clone(&rendezvous));
    let candidate = "blocked-candidate";
    let wait = Duration::from_millis(250);

    let (release_guard, guard_holder) = hold_adoption_lock(&lock_scope);
    let (allocated_tx, allocated_rx) = tokio::sync::oneshot::channel();
    let allocating_source = Arc::clone(&source);
    let allocating = std::thread::spawn(move || {
        let id = allocating_source.with_scope(|scope| scope.allocate(|_| false, candidate));
        let _ = allocated_tx.send(id);
    });
    assert!(
        tokio::time::timeout(wait, rendezvous.wait_until_parked()).await.is_err(),
        "an allocation must not reach its reservation window while the tree adoption lock is held"
    );
    drop(release_guard);
    guard_holder.join().expect("the holding thread releases the tree adoption lock");
    tokio::time::timeout(Duration::from_secs(5), rendezvous.wait_until_parked())
        .await
        .expect("the allocation acquires the lock and parks in the reservation window");
    rendezvous.release();
    assert_eq!(
        allocated_rx.await.expect("the allocation resumes"),
        candidate,
        "the waiting allocation must still take the free candidate"
    );
    allocating.join().expect("the allocating thread finishes");

    let (release_guard, guard_holder) = hold_adoption_lock(&lock_scope);
    let (adopted_tx, adopted_rx) = tokio::sync::oneshot::channel();
    let adopting_source = Arc::clone(&source);
    let adopting_target = Arc::clone(&target);
    let adopting = std::thread::spawn(move || {
        let outcome = adopting_source.adopt_into(&adopting_target);
        let _ = adopted_tx.send(outcome.is_ok());
    });
    assert!(
        tokio::time::timeout(wait, rendezvous.wait_for_adoption_lock_held())
            .await
            .is_err(),
        "a graft must not report holding the tree adoption lock while the guard is held"
    );
    drop(release_guard);
    guard_holder.join().expect("the holding thread releases the tree adoption lock");
    tokio::time::timeout(Duration::from_secs(5), rendezvous.wait_for_adoption_lock_held())
        .await
        .expect("the graft acquires the tree adoption lock");
    assert!(adopted_rx.await.expect("the graft resumes"), "the waiting graft must still succeed");
    adopting.join().expect("the adopting thread finishes");

    // Whichever step the lock admitted first, the id it recorded is the one
    // the grafted tree now reads, so the tree can never issue it again.
    let repeated = target.with_scope(|scope| scope.allocate(|_| false, candidate));
    assert_ne!(repeated, candidate, "the tree must refuse an id recorded on either side of the graft");
}

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
pub(in crate::subagents) struct AllocationRendezvous {
    parked_once: AtomicBool,
    entered: tokio::sync::Notify,
    merge_attempted: tokio::sync::Notify,
    adoption_lock_held: tokio::sync::Notify,
    reserved: AtomicBool,
    /// Set when the bounded park wait elapsed without a release.
    ///
    /// A test that reads this can tell its own gate timeout apart from lock
    /// contention, which would otherwise be reported for a reservation that
    /// never held the adoption lock.
    expired: AtomicBool,
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
    pub(super) fn park_once(&self) -> bool {
        if self.parked_once.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.entered.notify_one();
        let released = self.released.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // The wait is bounded so a failing test reports its assertion instead
        // of hanging a parked worker when the release never arrives. An
        // expired gate releases the allocation without the test choosing to,
        // so the fact is recorded rather than left to be misread as contention.
        let (released, outcome) = self
            .release
            .wait_timeout_while(released, Duration::from_secs(5), |released| !*released)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if outcome.timed_out() && !*released {
            self.expired.store(true, Ordering::SeqCst);
        }
        true
    }

    /// Reports whether a parked allocation resumed because its bounded wait
    /// elapsed instead of because the test released it.
    pub(in crate::subagents) fn expired(&self) -> bool {
        self.expired.load(Ordering::SeqCst)
    }

    /// Waits until the gated allocation has parked in the reservation window.
    pub(in crate::subagents) async fn wait_until_parked(&self) {
        self.entered.notified().await;
    }

    /// Records that the gated allocation reserved its id.
    pub(super) fn mark_reserved(&self) {
        self.reserved.store(true, Ordering::SeqCst);
    }

    /// Returns whether the gated allocation reserved its id before the merge.
    pub(in crate::subagents) fn reserved_before_merge(&self) -> bool {
        self.reserved.load(Ordering::SeqCst)
    }

    /// Signals that a graft has reached the adoption lock it must acquire.
    pub(super) fn signal_merge_attempt(&self) {
        self.merge_attempted.notify_one();
    }

    /// Waits until a graft has reached the adoption lock.
    pub(in crate::subagents) async fn wait_for_merge_attempt(&self) {
        self.merge_attempted.notified().await;
    }

    /// Signals that a graft now holds the adoption lock.
    pub(super) fn signal_adoption_lock_held(&self) {
        self.adoption_lock_held.notify_one();
    }

    /// Waits until a graft has acquired the adoption lock.
    pub(in crate::subagents) async fn wait_for_adoption_lock_held(&self) {
        self.adoption_lock_held.notified().await;
    }

    /// Releases a parked allocation so it can record its id.
    pub(in crate::subagents) fn release(&self) {
        *self.released.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
        self.release.notify_all();
    }
}

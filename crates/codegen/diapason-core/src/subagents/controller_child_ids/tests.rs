//! Pins the child-id allocation and subtree-graft contracts.
//!
//! A graft must leave every controller in the joined tree drawing from one id
//! space, a colliding id must never replace an admitted record, and allocation
//! and adoption must exclude each other through the tree's adoption lock.
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

/// Waits until a worker reports that it reached the adoption lock while held.
///
/// A worker that has not been scheduled yet is indistinguishable from one
/// blocked on the lock if the only evidence is that its operation has not
/// finished. Each worker therefore probes the lock itself and reports the
/// result, so the test knows it is at the disputed acquisition before it
/// asserts that the operation cannot complete.
fn await_lock_checkpoint(received: &std::sync::mpsc::Receiver<()>, worker: &str) {
    received
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|error| panic!("the {worker} must reach the adoption lock: {error}"));
}

/// Probes the adoption lock, returning whether it was already held.
///
/// The probe drops any guard it manages to take, so it leaves the lock in the
/// state it found.
fn adoption_lock_is_held(lock_scope: &SubagentLockScope) -> bool {
    let attempt = lock_scope.adoption().try_lock();
    attempt.is_err()
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
///
/// Each worker reports a checkpoint after it probes the held lock. Waiting for
/// that checkpoint proves the worker reached the disputed acquisition; without
/// it, an unscheduled worker would time out in exactly the same way as one
/// blocked on the lock. Removing either acquisition then fails the test twice
/// over: the checkpoint reports an unheld lock, and the operation completes
/// while the guard is still held.
#[test]
fn allocation_and_adoption_wait_for_the_tree_adoption_lock() {
    let lock_scope = Arc::new(SubagentLockScope::new());
    let source = Arc::new(ChildIdScopeSlot::with_lock_scope(Arc::clone(&lock_scope)));
    let target = Arc::new(ChildIdScopeSlot::with_lock_scope(Arc::clone(&lock_scope)));
    let candidate = "blocked-candidate";
    let wait = Duration::from_millis(250);

    let guard = lock_scope.adoption().lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let (allocating_at_lock_tx, allocating_at_lock_rx) = std::sync::mpsc::channel();
    let (allocated_tx, allocated_rx) = std::sync::mpsc::channel();
    let allocating_scope = Arc::clone(&lock_scope);
    let allocating_source = Arc::clone(&source);
    let allocating = std::thread::spawn(move || {
        let held = adoption_lock_is_held(&allocating_scope);
        let _ = allocating_at_lock_tx.send(());
        assert!(held, "the allocation must reach the adoption lock while it is held");
        let id = allocating_source.with_scope(|scope| scope.allocate(|_| false, candidate));
        let _ = allocated_tx.send(id);
    });
    await_lock_checkpoint(&allocating_at_lock_rx, "allocating worker");
    assert!(allocated_rx.recv_timeout(wait).is_err(), "an allocation must wait for the tree adoption lock");

    let (adopting_at_lock_tx, adopting_at_lock_rx) = std::sync::mpsc::channel();
    let (adopted_tx, adopted_rx) = std::sync::mpsc::channel();
    let adopting_scope = Arc::clone(&lock_scope);
    let adopting_source = Arc::clone(&source);
    let adopting_target = Arc::clone(&target);
    let adopting = std::thread::spawn(move || {
        let held = adoption_lock_is_held(&adopting_scope);
        let _ = adopting_at_lock_tx.send(());
        assert!(held, "the graft must reach the adoption lock while it is held");
        let outcome = adopting_source.adopt_into(&adopting_target);
        let _ = adopted_tx.send(outcome.is_ok());
    });
    await_lock_checkpoint(&adopting_at_lock_rx, "adopting worker");
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

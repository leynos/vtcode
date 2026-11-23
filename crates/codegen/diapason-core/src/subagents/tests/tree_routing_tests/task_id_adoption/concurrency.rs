//! Concurrent grafting and allocation: one graft must never repeat a task id.
use super::*;

use crate::subagents::controller_child_ids::AllocationRendezvous;

/// A graft and an allocation that run at the same time never repeat a task id.
///
/// The graft moves the joining subtree's reserved ids into the tree's allocator
/// and then rewrites the subtree's slot to point at it. An allocation that
/// resolves the subtree's allocator before that rewrite but records its id after
/// the merge would leave the tree without that id, so the tree would issue it
/// again and the adapter would list one task twice. The shared adoption lock
/// must therefore cover the whole reconciliation on both sides.
///
/// The allocation side of that race is exercised through a controller the
/// joining subtree already holds, not through the joining controller itself.
/// A controller allocates through its own slot, so only a controller whose slot
/// forwards to the allocator being merged can resolve it before the rewrite and
/// record into it afterwards; the joining controller's own slot would serialise
/// the two operations by itself.
///
/// The detection has to run from the tree's own controller. An allocation
/// consults the allocating controller's local child map as well as the shared
/// allocator, so every id the subtree issues lands in its own map and its own
/// later allocations would refuse them even if the tree never learned them. The
/// tree's map holds none of them, so only a sweep from the tree shows whether
/// the allocator it now owns really received every reserved id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_graft_and_allocation_never_repeat_a_task_id() {
    const ROUNDS: usize = 8;
    const PRE_GRAFT_IDS: usize = 400;
    const RACING_ALLOCATIONS: usize = 200;
    const POST_GRAFT_ALLOCATIONS: usize = 200;

    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let candidate = "agent-explorer-20261118T053000000Z";

    for round in 0..ROUNDS {
        // A real nested tree shares one lock scope; giving every controller the
        // same scope is what lets the graft be accepted while still binding the
        // test's own allocators rather than the process-global ones.
        let scope = Arc::new(SubagentLockScope::new());
        let mut root_config = test_controller_config(workspace_root.clone(), DiapasonConfig::default());
        root_config.lock_scope = Some(Arc::clone(&scope));
        let root = Arc::new(SubagentController::new(root_config).await.expect("root controller"));

        let joining_session = format!("joining-session-{round}");
        let mut joining_config = test_controller_config(workspace_root.clone(), DiapasonConfig::default());
        joining_config.parent_session_id = joining_session.clone();
        joining_config.depth = 1;
        joining_config.managed_background_runtime = true;
        joining_config.lock_scope = Some(Arc::clone(&scope));
        let joining = Arc::new(SubagentController::new(joining_config).await.expect("joining controller"));

        // The racing allocations come from a controller the joining subtree
        // already holds. Its slot is its own, forwarding to the allocator the
        // raced graft is about to merge and redirect, so it resolves that
        // allocator without taking the lock on the joining controller's own
        // slot. A race driven through the joining controller itself would be
        // serialised by that slot's own read/write lock and would prove nothing
        // about the adoption lock.
        let descendant_session = format!("descendant-session-{round}");
        let mut descendant_config = test_controller_config(workspace_root.clone(), DiapasonConfig::default());
        descendant_config.parent_session_id = descendant_session.clone();
        descendant_config.depth = 2;
        descendant_config.managed_background_runtime = true;
        descendant_config.lock_scope = Some(Arc::clone(&scope));
        let descendant = Arc::new(SubagentController::new(descendant_config).await.expect("descendant controller"));
        let spec = Arc::new(explorer_spec());

        joining.state.write().await.children.insert(
            "descendant".to_string(),
            test_child_record(
                "descendant",
                &descendant_session,
                &joining_session,
                spec.as_ref(),
                SubagentStatus::Running,
                2,
                None,
            ),
        );
        joining
            .attach_child_controller("descendant", Arc::clone(&descendant))
            .await
            .expect("the descendant joins the joining subtree before the raced graft");

        // Pre-fill the joining subtree so the graft's merge has real work to do
        // while the other thread reserves ids.
        for ordinal in 0..PRE_GRAFT_IDS {
            let id = format!("pre-graft-{round}-{ordinal}");
            let issued = joining
                .insert_child_with_candidate_id(
                    &id,
                    test_child_record(
                        &id,
                        &format!("{id}-session"),
                        &joining_session,
                        spec.as_ref(),
                        SubagentStatus::Queued,
                        2,
                        None,
                    ),
                )
                .await
                .expect("pre-fill the joining subtree");
            assert_eq!(issued, id, "a distinct pre-graft id must be issued verbatim");
        }

        root.state.write().await.children.insert(
            "joining".to_string(),
            test_child_record(
                "joining",
                "joining-session",
                "parent-session",
                spec.as_ref(),
                SubagentStatus::Running,
                1,
                None,
            ),
        );

        // Park the racer's first reservation between resolving its terminal
        // scope and recording its id. The allocation already holds the
        // adoption lock at that point, so a graft cannot merge until the id is
        // recorded. The gate removes the scheduling race this test used to
        // rely on: without it, the racer's first poll could happen only after
        // the graft had already completed.
        let rendezvous = Arc::new(AllocationRendezvous::default());
        descendant
            .child_id_slot()
            .install_allocation_rendezvous(Arc::clone(&rendezvous));
        joining.child_id_slot().install_allocation_rendezvous(Arc::clone(&rendezvous));

        // A second worker thread reserves ids through the descendant while the
        // graft reconciles the two allocators.
        let racing = {
            let descendant = Arc::clone(&descendant);
            let spec = Arc::clone(&spec);
            let descendant_session = descendant_session.clone();
            tokio::spawn(async move {
                let mut issued = Vec::with_capacity(RACING_ALLOCATIONS);
                for ordinal in 0..RACING_ALLOCATIONS {
                    let session = format!("{descendant_session}-racing-{ordinal}");
                    issued.push(
                        descendant
                            .insert_child_with_candidate_id(
                                candidate,
                                test_child_record(
                                    candidate,
                                    &session,
                                    &descendant_session,
                                    spec.as_ref(),
                                    SubagentStatus::Queued,
                                    3,
                                    None,
                                ),
                            )
                            .await
                            .expect("allocate while the graft runs"),
                    );
                }
                issued
            })
        };

        rendezvous.wait_until_parked().await;
        // Drive the graft's merge directly. The async attachment path first
        // walks the joining subtree's state, and the parked allocation holds
        // the descendant's state write lock, so that walk would block before
        // reaching the adoption lock and would not isolate the reservation
        // window. `adopt_into` is the operation whose exclusion of that window
        // this test is about, and the blocking pool keeps both the parked
        // allocation and the blocked merge from starving runtime workers.
        let mut graft = {
            let joining_slot = joining.child_id_slot();
            let root_slot = root.child_id_slot();
            tokio::task::spawn_blocking(move || joining_slot.adopt_into(&root_slot))
        };
        rendezvous.wait_for_merge_attempt().await;
        let contended = tokio::time::timeout(Duration::from_millis(100), &mut graft).await;
        // An expired gate released the reservation without the test releasing
        // it, so the contention check below would report a lock that was never
        // held. Fail with the timeout that actually happened instead.
        assert!(
            !rendezvous.expired(),
            "round {round}: the reservation gate expired instead of holding the adoption lock"
        );
        assert!(contended.is_err(), "round {round}: the graft must block behind the in-flight reservation");
        rendezvous.release();
        graft
            .await
            .expect("the graft task must finish")
            .expect("the joining subtree shares the tree's adoption lock");
        assert!(
            rendezvous.reserved_before_merge(),
            "round {round}: the reservation must be recorded before the graft merges"
        );
        let racing_ids = racing.await.expect("the racing allocator must finish");
        let mut issued = racing_ids.clone();

        // The grafted subtree must draw from the tree's allocator once the
        // graft completes, so its own later allocations continue the sequence.
        for ordinal in 0..POST_GRAFT_ALLOCATIONS {
            let session = format!("{joining_session}-after-{ordinal}");
            issued.push(
                joining
                    .insert_child_with_candidate_id(
                        candidate,
                        test_child_record(
                            candidate,
                            &session,
                            &joining_session,
                            spec.as_ref(),
                            SubagentStatus::Queued,
                            2,
                            None,
                        ),
                    )
                    .await
                    .expect("allocate after the graft"),
            );
        }

        // Every id the subtree reserved during the graft is now known to the
        // tree, so proposing the same candidate again must not repeat any of
        // them. The sweep runs on the tree's controller, whose local child map
        // holds none of the subtree's records: a reservation the merge failed
        // to copy is therefore visible here as an id issued twice, instead of
        // being masked by the map of the controller that issued it.
        let mut tree_ids = Vec::with_capacity(POST_GRAFT_ALLOCATIONS);
        for ordinal in 0..POST_GRAFT_ALLOCATIONS {
            let session = format!("parent-session-after-{ordinal}");
            tree_ids.push(
                root.insert_child_with_candidate_id(
                    candidate,
                    test_child_record(
                        candidate,
                        &session,
                        "parent-session",
                        spec.as_ref(),
                        SubagentStatus::Queued,
                        1,
                        None,
                    ),
                )
                .await
                .expect("allocate from the tree after the graft"),
            );
        }

        let reissued = tree_ids.iter().filter(|id| issued.contains(id)).cloned().collect::<Vec<_>>();
        assert!(
            reissued.is_empty(),
            "round {round}: the tree re-issued task ids the joining subtree had already reserved: {reissued:?}"
        );

        issued.extend(tree_ids);
        let mut seen = std::collections::BTreeSet::new();
        let repeated = issued
            .iter()
            .filter(|id| !seen.insert((*id).clone()))
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            repeated.is_empty(),
            "round {round} issued these task ids more than once across the graft: {repeated:?}"
        );

        joining.signal_shutdown().await;
        root.signal_shutdown().await;
    }
}

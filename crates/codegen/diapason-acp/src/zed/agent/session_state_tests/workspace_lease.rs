//! Workspace-lease accounting and runtime lifecycle tests.
use super::*;
use assert_fs::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_lease_accounting_matches_the_reference_model() {
    use proptest::prelude::*;

    let launch_workspace = TempDir::new().expect("launch workspace");
    let session_workspace = TempDir::new().expect("session workspace");
    let agent = Arc::new(build_agent(launch_workspace.path()).await);
    let approved_workspace = AcpSessionWorkspace::from_client_cwd(session_workspace.path())
        .await
        .expect("approve session workspace");

    let strategy = prop::collection::vec(
        prop_oneof![
            Just(LeaseOp::Acquire),
            Just(LeaseOp::AcquireUnmanaged),
            Just(LeaseOp::Release),
            Just(LeaseOp::Drop),
        ],
        0..12,
    );

    let handle = tokio::runtime::Handle::current();

    proptest!(ProptestConfig { cases: 32, max_shrink_iters: 256, ..ProptestConfig::default() },
        |(ops in strategy)| {
            let mut model = LeaseModel::default();
            let mut live: Vec<SessionWorkspaceLease> = Vec::new();

            let managed = tokio::task::block_in_place(|| {
                handle.block_on(isolated_runtime(true, session_workspace.path(), &agent))
            });
            let unmanaged = tokio::task::block_in_place(|| {
                handle.block_on(isolated_runtime(false, session_workspace.path(), &agent))
            });

            for op in ops {
                match op {
                    LeaseOp::Acquire => {
                        let model_grants = model.acquire();
                        let granted = bounded(&handle, SessionWorkspaceLease::acquire(Arc::clone(&managed)))
                            .expect("acquiring a managed lease must not fail");
                        prop_assert_eq!(
                            granted.is_some(),
                            model_grants,
                            "acquisition must match the model, which expects {:?}",
                            model.state
                        );
                        if let Some(lease) = granted {
                            live.push(lease);
                        }
                    }
                    LeaseOp::AcquireUnmanaged => {
                        let lease = bounded(&handle, SessionWorkspaceLease::acquire(Arc::clone(&unmanaged)))
                            .expect("acquiring an unmanaged lease must not fail")
                        .expect("unmanaged runtimes always grant a lease");
                        prop_assert!(
                            !lease.is_counted(),
                            "an unmanaged runtime must hand back an uncounted lease"
                        );
                        drop(lease);
                        prop_assert_eq!(
                            active_sessions(&unmanaged),
                            0,
                            "unmanaged leases must never touch unmanaged accounting"
                        );
                    }
                    LeaseOp::Release => {
                        if let Some(lease) = live.pop() {
                            bounded(&handle, lease.release());
                            model.leave(true);
                        }
                    }
                    LeaseOp::Drop => {
                        if let Some(lease) = live.pop() {
                            drop(lease);
                            model.leave(false);
                        }
                    }
                }

                prop_assert_eq!(
                    active_sessions(&managed),
                    model.active_sessions,
                    "live counted leases must match the model after {:?}",
                    op
                );
                let state = lifecycle_state(&managed);
                prop_assert!(
                    model.admits(state),
                    "runtime state {state:?} is not admitted by model state {:?} after {:?}",
                    model.state,
                    op
                );
            }

            // Drain, asserting each departure decrements exactly once. `release`
            // consumes the lease, so the following `Drop` cannot decrement the
            // same handle a second time.
            while let Some(lease) = live.pop() {
                let before = active_sessions(&managed);
                bounded(&handle, lease.release());
                prop_assert_eq!(
                    active_sessions(&managed),
                    before - 1,
                    "explicit release must decrement exactly one accounted lease"
                );
                model.leave(true);
            }
            prop_assert_eq!(active_sessions(&managed), 0, "the drained runtime must reach zero");
            let drained_state = lifecycle_state(&managed);
            prop_assert!(
                model.admits(drained_state),
                "runtime state {drained_state:?} is not admitted by model state {:?} after draining",
                model.state
            );
        }
    );

    // Deterministic: a terminal runtime refuses new leases, and that refusal is
    // distinct from the agent manufacturing a replacement runtime for the same
    // workspace.
    let lease = agent
        .workspace_runtime_for(&approved_workspace)
        .await
        .expect("the agent must create a workspace runtime");
    let runtime = Arc::clone(lease.runtime());
    assert!(lease.is_counted(), "a cached workspace runtime lease is counted");
    assert_eq!(active_sessions(&runtime), 1);

    runtime.mark_shutdown_complete();
    let refused = SessionWorkspaceLease::acquire(Arc::clone(&runtime))
        .await
        .expect("acquiring from a terminal runtime must not error");
    assert!(refused.is_none(), "a terminal runtime must not grant another counted lease");
    assert_eq!(active_sessions(&runtime), 1, "a refused acquisition must leave the count untouched");

    lease.release().await;
    assert_eq!(active_sessions(&runtime), 0, "explicit release must decrement exactly one accounted lease");
    assert_eq!(
        lifecycle_state(&runtime),
        WorkspaceRuntimeLifecycleState::Closed,
        "an explicit release after terminal shutdown must not reopen the lifecycle"
    );

    let replacement = agent
        .workspace_runtime_for(&approved_workspace)
        .await
        .expect("the agent must replace a terminal runtime");
    assert!(
        !Arc::ptr_eq(replacement.runtime(), &runtime),
        "workspace_runtime_for must not hand back a terminal runtime"
    );
    assert_eq!(active_sessions(replacement.runtime()), 1, "the replacement runtime must start a fresh count");
    replacement.release().await;
    drop(runtime);
}

/// Explicit release consumes the lease, so its later destruction cannot
/// decrement the count a second time or revive a finished lifecycle.
#[tokio::test]
async fn explicit_release_then_destruction_does_not_decrement_twice() {
    let launch_workspace = TempDir::new().expect("launch workspace");
    let session_workspace = TempDir::new().expect("session workspace");
    let agent = Arc::new(build_agent(launch_workspace.path()).await);

    let runtime = isolated_runtime(true, session_workspace.path(), &agent).await;
    let keeper = SessionWorkspaceLease::acquire(Arc::clone(&runtime))
        .await
        .expect("acquiring a managed lease must not fail")
        .expect("an open runtime must grant a lease");
    let released = SessionWorkspaceLease::acquire(Arc::clone(&runtime))
        .await
        .expect("acquiring a second managed lease must not fail")
        .expect("an open runtime must grant a lease");
    assert_eq!(active_sessions(&runtime), 2);

    // Releasing one of two leaves the count and the lifecycle untouched.
    released.release().await;
    assert_eq!(active_sessions(&runtime), 1);
    assert_eq!(lifecycle_state(&runtime), WorkspaceRuntimeLifecycleState::Open);

    // The final lease drives shutdown to completion before `release` returns.
    keeper.release().await;
    assert_eq!(active_sessions(&runtime), 0);
    assert_eq!(lifecycle_state(&runtime), WorkspaceRuntimeLifecycleState::Closed);

    // Dropping the runtime handle now must not re-run either decrement.
    drop(runtime);

    // A `Drop`-only departure publishes `ShutdownPending` and stops there.
    let dropped_runtime = isolated_runtime(true, session_workspace.path(), &agent).await;
    let lease = SessionWorkspaceLease::acquire(Arc::clone(&dropped_runtime))
        .await
        .expect("acquiring a managed lease must not fail")
        .expect("an open runtime must grant a lease");
    drop(lease);
    assert_eq!(active_sessions(&dropped_runtime), 0);
    assert_eq!(
        lifecycle_state(&dropped_runtime),
        WorkspaceRuntimeLifecycleState::ShutdownPending,
        "a dropped final lease must request shutdown without driving it"
    );
}

/// Acquiring from a runtime whose final lease went through `Drop` must initiate
/// shutdown, wait for it to finish and refuse the caller; the now-terminal
/// runtime must keep refusing afterwards.
#[tokio::test]
async fn acquisition_from_a_pending_shutdown_closes_the_runtime_and_refuses() {
    let launch_workspace = TempDir::new().expect("launch workspace");
    let session_workspace = TempDir::new().expect("session workspace");
    let agent = Arc::new(build_agent(launch_workspace.path()).await);

    let runtime = isolated_runtime(true, session_workspace.path(), &agent).await;
    let lease = SessionWorkspaceLease::acquire(Arc::clone(&runtime))
        .await
        .expect("acquiring a managed lease must not fail")
        .expect("an open runtime must grant a lease");
    drop(lease);
    assert_eq!(active_sessions(&runtime), 0);
    assert_eq!(
        lifecycle_state(&runtime),
        WorkspaceRuntimeLifecycleState::ShutdownPending,
        "the drop must leave shutdown requested but not initiated"
    );

    let refused = tokio::time::timeout(LEASE_DEADLINE, SessionWorkspaceLease::acquire(Arc::clone(&runtime)))
        .await
        .expect("acquisition must not stall waiting for the shutdown it initiated")
        .expect("acquiring from a pending runtime must not error");
    assert!(refused.is_none(), "a pending runtime must refuse the old runtime's lease");
    assert_eq!(
        lifecycle_state(&runtime),
        WorkspaceRuntimeLifecycleState::Closed,
        "acquisition must drive the pending shutdown to completion"
    );
    assert_eq!(active_sessions(&runtime), 0, "a refused acquisition must not be counted");

    let refused_again = tokio::time::timeout(LEASE_DEADLINE, SessionWorkspaceLease::acquire(Arc::clone(&runtime)))
        .await
        .expect("acquiring from a terminal runtime must answer without waiting")
        .expect("acquiring from a terminal runtime must not error");
    assert!(refused_again.is_none(), "a terminal runtime must keep refusing leases");
    assert_eq!(lifecycle_state(&runtime), WorkspaceRuntimeLifecycleState::Closed);
}

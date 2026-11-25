//! The lifecycle clock handoff: no revision may be handed out twice.
//!
//! A graft copies the joining controller's clock into the tree's clock and then
//! moves the controller onto it. These cases pin the invariant that makes the
//! copy safe, as a concurrent stress loop and as a deterministic counterpart
//! that parks a reservation inside its read window. Declared by
//! `mod clock_handoff;` from `tests.rs`, which keeps this concern apart from the
//! attachment and routing regressions.

use super::*;
use crate::subagents::controller_observers::ProgressObserverRegistry;

/// A reservation that races the clock handoff never repeats a revision.
///
/// The handoff copies the joining controller's clock into the tree's clock
/// and then moves the controller onto it. A reservation that read the old
/// clock before the copy and incremented it afterwards would hand out a
/// value the copy had already absorbed, so the tree would publish that
/// revision twice and an adapter that drops non-increasing revisions would
/// discard one of the two snapshots.
#[tokio::test]
async fn concurrent_reserves_across_a_clock_handoff_never_repeat_a_revision() {
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    const RESERVERS: usize = 3;
    const RESERVES_PER_RESERVER: usize = 20_000;
    // Bounds the handoff loop so a reserver that panicked reports its
    // failure through the join instead of parking the test forever.
    const HANDOFF_LIMIT: usize = 200_000;

    let temp = TempDir::new().expect("tempdir");
    let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf()))
        .await
        .expect("controller");
    let registry = Arc::new(ProgressObserverRegistry::default());
    let running = AtomicUsize::new(RESERVERS);

    let reserved = std::thread::scope(|scope| {
        let mut reservers = Vec::with_capacity(RESERVERS);
        for _ in 0..RESERVERS {
            let controller = &controller;
            let running = &running;
            reservers.push(scope.spawn(move || {
                let mut revisions = Vec::with_capacity(RESERVES_PER_RESERVER);
                for _ in 0..RESERVES_PER_RESERVER {
                    revisions.push(controller.reserve_lifecycle_revision());
                }
                running.fetch_sub(1, Ordering::SeqCst);
                revisions
            }));
        }
        // Each handoff retires the clock it copies and installs a fresh
        // one, so every iteration opens the window the fix has to close.
        let mut handoffs = 0_usize;
        while running.load(Ordering::SeqCst) > 0 && handoffs < HANDOFF_LIMIT {
            let clock = Arc::new(AtomicU64::new(0));
            controller.progress_route.adopt(Arc::clone(&registry), None, clock);
            handoffs += 1;
            std::hint::spin_loop();
        }
        reservers
            .into_iter()
            .flat_map(|reserver| reserver.join().expect("a reserver must not panic"))
            .collect::<Vec<u64>>()
    });

    let mut distinct = reserved.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        reserved.len(),
        "a handoff handed out {} of {} revisions twice",
        reserved.len() - distinct.len(),
        reserved.len()
    );
    assert_eq!(
        distinct.last().copied(),
        Some(reserved.len() as u64 - 1),
        "the revisions must stay contiguous across every handoff"
    );
}

/// A lifecycle reservation parked in its read window excludes a clock handoff.
///
/// The stress loop above can pass without ever exercising a handoff: every
/// reservation may finish before an adoption starts, so the test would not
/// notice a handoff that stopped excluding the reservation window. This case
/// parks one reservation inside the route's read guard, so the handoff must
/// wait for the write guard, and then checks that the adopted clock carries the
/// parked revision forward instead of handing it out a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parked_reservation_excludes_the_clock_handoff() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use crate::subagents::controller_observers::ReserveRendezvous;

    let temp = TempDir::new().expect("tempdir");
    let controller = Arc::new(
        SubagentController::new(test_controller_config(temp.path().to_path_buf()))
            .await
            .expect("controller"),
    );
    let rendezvous = Arc::new(ReserveRendezvous::default());
    controller
        .progress_route
        .install_lifecycle_reserve_rendezvous(Arc::clone(&rendezvous));

    // The reservation parks after taking the route's read guard.
    let reserver = {
        let controller = Arc::clone(&controller);
        tokio::task::spawn_blocking(move || controller.reserve_lifecycle_revision())
    };
    tokio::time::timeout(Duration::from_secs(5), rendezvous.wait_until_parked())
        .await
        .expect("the reservation must reach the parked checkpoint before the handoff starts");
    assert!(!rendezvous.expired(), "the reservation gate expired before the handoff started");

    // The handoff needs the write guard the parked reservation holds. "It had
    // not finished after 100 ms" is weak evidence on its own: a descheduled
    // thread looks the same. Three facts together are decisive. The gate is
    // still parked, so no lock timeout released the reservation early; the
    // handoff has not returned, so it cannot have taken the guard; and once
    // released the handoff's `fetch_max` carries the parked revision, which it
    // can only copy by acquiring the guard after that reservation recorded it.
    let registry = Arc::new(ProgressObserverRegistry::default());
    let adopted_clock = Arc::new(AtomicU64::new(0));
    let completed = Arc::new(AtomicU64::new(0));
    let (attempted_tx, mut attempted_rx) = tokio::sync::mpsc::unbounded_channel();
    let (adopted_tx, mut adopted_rx) = tokio::sync::mpsc::unbounded_channel();
    let handoff = {
        let controller = Arc::clone(&controller);
        let clock = Arc::clone(&adopted_clock);
        let completed = Arc::clone(&completed);
        tokio::task::spawn_blocking(move || {
            let _ = attempted_tx.send(());
            controller.progress_route.adopt(Arc::clone(&registry), None, clock);
            let _ = completed.fetch_add(1, Ordering::SeqCst);
            let _ = adopted_tx.send(());
        })
    };
    attempted_rx.recv().await.expect("the handoff must start");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), adopted_rx.recv())
            .await
            .is_err(),
        "the handoff must block while a reservation holds the route's read guard"
    );
    assert!(!rendezvous.expired(), "the reservation gate must still be parked while the handoff waits");
    assert_eq!(
        completed.load(Ordering::SeqCst),
        0,
        "the handoff must not have reached the contested lock while the reservation holds its read guard"
    );

    rendezvous.release();
    let parked = reserver.await.expect("the reservation must finish");
    adopted_rx
        .recv()
        .await
        .expect("the handoff must finish once the reservation is recorded");
    handoff.await.expect("the handoff task must finish");
    assert_eq!(
        completed.load(Ordering::SeqCst),
        1,
        "the handoff must have returned once the reservation released the lock it contested"
    );

    assert_eq!(parked, 0, "the parked reservation takes the first revision");
    assert_eq!(
        adopted_clock.load(Ordering::Relaxed),
        parked + 1,
        "the handoff must carry the parked revision into the adopted clock"
    );
    assert_eq!(
        controller.reserve_lifecycle_revision(),
        parked + 1,
        "the next revision must follow the parked one exactly"
    );
}

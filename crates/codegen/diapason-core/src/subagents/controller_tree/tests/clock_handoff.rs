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
/// parks one reservation inside the route's read guard and then probes the
/// write guard the handoff needs.
///
/// Waiting for the handoff task to report that it started proves nothing about
/// the lock: a worker that signals before it calls `adopt` can be descheduled
/// for the whole parked window, and a worker that is merely slow looks the
/// same as one that is excluded. The probe therefore takes the write lock
/// itself, without waiting for it, from the test's own thread. While the gate
/// is parked the attempt must be refused, and after the release the same
/// attempt must succeed, so the refusal is the parked reservation's read guard
/// and nothing else. The revision assertions then pin that the handoff copied
/// the parked revision rather than handing it out a second time.
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

    // The parked reservation holds the read guard, so the write guard the
    // handoff needs cannot be taken. This attempt does not wait for the lock
    // and does not depend on the handoff task having been scheduled: it asks
    // the route itself, on the test's thread, whether the write guard is free.
    assert!(
        !controller.progress_route.write_guard_available(),
        "a reservation parked in its read window must exclude the handoff's write guard"
    );
    assert!(!rendezvous.expired(), "the reservation gate must still be parked while the probe is refused");

    // The handoff blocks on that same guard. Its completion is recorded so the
    // test can prove it had not finished while the reservation was parked,
    // but the probe above, not this counter, is the evidence of exclusion.
    let registry = Arc::new(ProgressObserverRegistry::default());
    let adopted_clock = Arc::new(AtomicU64::new(0));
    let completed = Arc::new(AtomicU64::new(0));
    let (adopted_tx, mut adopted_rx) = tokio::sync::mpsc::unbounded_channel();
    let handoff = {
        let controller = Arc::clone(&controller);
        let clock = Arc::clone(&adopted_clock);
        let completed = Arc::clone(&completed);
        tokio::task::spawn_blocking(move || {
            controller.progress_route.adopt(Arc::clone(&registry), None, clock);
            let _ = completed.fetch_add(1, Ordering::SeqCst);
            let _ = adopted_tx.send(());
        })
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(100), adopted_rx.recv())
            .await
            .is_err(),
        "the handoff must not finish while a reservation holds the route's read guard"
    );
    assert!(!rendezvous.expired(), "the reservation gate must still be parked while the handoff waits");
    assert_eq!(
        completed.load(Ordering::SeqCst),
        0,
        "the handoff must not have reached the contested lock while the reservation holds its read guard"
    );

    rendezvous.release();
    let parked = reserver.await.expect("the reservation must finish");

    // The same probe that was refused while parked must now be accepted. Were
    // the earlier refusal a scheduling artefact rather than the read guard,
    // this attempt could still be refused; were the guard available while
    // parked, the earlier assertion would already have failed.
    let available = tokio::time::timeout(Duration::from_secs(5), async {
        let mut available = controller.progress_route.write_guard_available();
        while !available {
            tokio::task::yield_now().await;
            available = controller.progress_route.write_guard_available();
        }
    })
    .await;
    assert!(
        available.is_ok(),
        "the write guard must become available once the parked reservation records its revision"
    );

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

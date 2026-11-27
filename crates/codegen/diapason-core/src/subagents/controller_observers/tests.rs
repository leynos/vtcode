//! Regressions for replacing a controller's publication route.
//!
//! The reservation gate lives here because production never installs one: only
//! a regression that must prove a clock handoff excludes a reservation parks
//! the first reservation inside the route's read guard.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A test-only gate around one reservation's read-guard window.
///
/// A test that must prove a clock handoff excludes a reservation parks the
/// first reservation after it takes the route's read guard and before it
/// increments the clock. The handoff needs the write guard, so it cannot copy
/// the revision the parked reservation is about to take; releasing the gate
/// lets the reservation record its revision first, and the handoff's
/// `fetch_max` carries it into the adopted clock.
#[derive(Default)]
pub(in crate::subagents) struct ReserveRendezvous {
    parked_once: AtomicBool,
    entered: tokio::sync::Notify,
    expired: AtomicBool,
    released: Mutex<bool>,
    release: std::sync::Condvar,
}

impl ReserveRendezvous {
    /// Parks the first reservation that reaches the gate until [`Self::release`].
    pub(in crate::subagents) fn park_once(&self) {
        if self.parked_once.swap(true, Ordering::SeqCst) {
            return;
        }
        self.entered.notify_one();
        // The wait is bounded so a failing test reports its assertion instead
        // of hanging the parked thread. An expired gate is recorded so the
        // test can tell its own timeout apart from lock contention.
        let released = self.released.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (released, outcome) = self
            .release
            .wait_timeout_while(released, std::time::Duration::from_secs(30), |released| !*released)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if outcome.timed_out() && !*released {
            self.expired.store(true, Ordering::SeqCst);
        }
    }

    /// Waits until the gated reservation is parked in its read-guard window.
    pub(in crate::subagents) async fn wait_until_parked(&self) {
        self.entered.notified().await;
    }

    /// Releases the parked reservation so it can record its revision.
    pub(in crate::subagents) fn release(&self) {
        *self.released.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        self.release.notify_all();
    }

    /// Reports whether the bounded wait elapsed without a release.
    pub(in crate::subagents) fn expired(&self) -> bool {
        self.expired.load(Ordering::SeqCst)
    }
}

impl ProgressObserverRoute {
    /// Installs the gate that parks one reservation, for the clock-handoff
    /// regression.
    pub(in crate::subagents) fn install_lifecycle_reserve_rendezvous(&self, rendezvous: Arc<ReserveRendezvous>) {
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reserve_rendezvous = Some(rendezvous);
    }

    /// Reports whether this route's write guard is currently held.
    ///
    /// The probe calls this from an observer's `Drop` to prove the retired
    /// route is released outside the guard that `reserve` contends with.
    fn write_guard_held(&self) -> bool {
        self.state.try_read().is_err()
    }

    /// Tries to take this route's write guard without waiting for it.
    ///
    /// A test that must prove a parked reservation excludes a handoff calls
    /// this on its own thread. The answer therefore does not depend on whether
    /// another task has been scheduled yet: the guard is available only while
    /// no reader or writer holds it. `reserve` holds the read guard for its
    /// whole parked window, so a refusal taken while the gate is parked is
    /// evidence of that reservation, and the same attempt succeeding after the
    /// release is evidence the window closed.
    pub(in crate::subagents) fn write_guard_available(&self) -> bool {
        self.state.try_write().is_ok()
    }
}

/// An observer that reports whether the route was still write-locked when the
/// registry owning it was dropped.
struct RetiredRouteProbe {
    route: ProgressObserverRoute,
    dropped: Arc<AtomicBool>,
    released_outside_guard: Arc<AtomicBool>,
}

impl SubagentProgressObserver for RetiredRouteProbe {
    fn subagent_status_changed(&self, _entry: SubagentStatusEntry) {}

    fn background_status_changed(&self, _entry: BackgroundSubprocessEntry) {}
}

impl Drop for RetiredRouteProbe {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
        self.released_outside_guard
            .store(!self.route.write_guard_held(), Ordering::SeqCst);
    }
}

/// A retired route drops the observers it cannot carry outside the write guard.
///
/// `adopt` replaces the registry this controller publishes through, and it
/// carries into the adopted registry every registration an adapter still holds
/// a handle for. A registration whose handle is already gone cannot be carried:
/// the adopted registry would hold an observer no handle could ever detach, so
/// it is released with the retired state instead. That release happens only
/// after the write guard is dropped; `reserve` takes the same route's read
/// guard, so releasing it under the guard would block every lifecycle revision
/// behind observer teardown.
#[test]
fn a_retired_route_drops_its_observers_outside_the_write_guard() {
    let route = ProgressObserverRoute::default();
    let registry = Arc::new(ProgressObserverRegistry::default());
    let dropped = Arc::new(AtomicBool::new(false));
    let released_outside_guard = Arc::new(AtomicBool::new(false));
    let probe: Arc<dyn SubagentProgressObserver> = Arc::new(RetiredRouteProbe {
        route: route.clone(),
        dropped: Arc::clone(&dropped),
        released_outside_guard: Arc::clone(&released_outside_guard),
    });
    {
        // A registration no handle owns: its location is dropped, which is what
        // a handle that drops while a swap is in flight leaves behind.
        let location = RegistrationLocation::new(&registry, 0);
        let mut observers = registry.observers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        observers.insert(
            0,
            RegisteredProgressObserver {
                parent_session_id: "parent-session".to_string(),
                observer: probe,
                location: Arc::downgrade(&location),
            },
        );
    }

    // Install the probe registry. This first swap retires the default route,
    // which owns no observers.
    route.adopt(Arc::clone(&registry), Some("parent-session".to_string()), Arc::new(AtomicU64::new(0)));
    drop(registry);
    assert!(!dropped.load(Ordering::SeqCst), "the installed observer must stay alive");

    // The next swap cannot carry a registration no handle owns, so retiring
    // this route releases the observer with the retired state.
    route.adopt(Arc::new(ProgressObserverRegistry::default()), None, Arc::new(AtomicU64::new(0)));

    assert!(dropped.load(Ordering::SeqCst), "the retired route must drop the observers it cannot carry");
    assert!(
        released_outside_guard.load(Ordering::SeqCst),
        "the retired route must be dropped after the write guard is released"
    );
}

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

/// A retired route drops its registry and observers outside the write guard.
///
/// `adopt` replaces the registry this controller publishes through. The retired
/// route owns the last reference to the previous registry, so dropping it drops
/// the observers registered there. `reserve` takes the same route's read guard,
/// so releasing the retired state under the write guard would block every
/// lifecycle revision behind observer teardown.
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
        let mut observers = registry.observers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        observers.insert(
            0,
            RegisteredProgressObserver {
                parent_session_id: "parent-session".to_string(),
                observer: probe,
            },
        );
    }

    // Install the probe registry. This first swap retires the default route,
    // which owns no observers.
    route.adopt(Arc::clone(&registry), Some("parent-session".to_string()), Arc::new(AtomicU64::new(0)));
    drop(registry);
    assert!(!dropped.load(Ordering::SeqCst), "the installed observer must stay alive");

    // Retiring this route drops the last reference to the registry and the
    // observer it owns.
    route.adopt(Arc::new(ProgressObserverRegistry::default()), None, Arc::new(AtomicU64::new(0)));

    assert!(dropped.load(Ordering::SeqCst), "the retired route must drop its observers");
    assert!(
        released_outside_guard.load(Ordering::SeqCst),
        "the retired route must be dropped after the write guard is released"
    );
}

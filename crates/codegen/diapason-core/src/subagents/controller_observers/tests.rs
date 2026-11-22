//! Regressions for replacing a controller's publication route.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

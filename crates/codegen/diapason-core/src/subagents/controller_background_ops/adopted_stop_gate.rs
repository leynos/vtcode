//! A test gate between adopting a persisted stop and closing its launch.
//!
//! The adopted launch's id is captured under the state lock, but the close runs
//! after that lock (and the ledger lock) is released. A concurrent start can
//! take the record in that window, and no real scheduling makes the race
//! deterministic, so a test arms this gate to land the replacement start and
//! then observe which session the close addresses.

use super::*;

struct AdoptedStopCloseGate {
    state: Weak<RwLock<ControllerState>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

fn adopted_stop_close_gates() -> &'static Mutex<Vec<AdoptedStopCloseGate>> {
    static GATES: std::sync::OnceLock<Mutex<Vec<AdoptedStopCloseGate>>> = std::sync::OnceLock::new();
    GATES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Arms a gate that holds the next adopted-stop close of `state`.
pub(super) fn gate_next_adopted_background_stop_close(
    state: &Arc<RwLock<ControllerState>>,
) -> (Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut gates = adopted_stop_close_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gates.retain(|gate| gate.state.strong_count() > 0);
    gates.push(AdoptedStopCloseGate {
        state: Arc::downgrade(state),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    (entered, release)
}

/// Parks here when `state` has an armed adopted-stop close gate.
pub(super) async fn pass(state: &Arc<RwLock<ControllerState>>) {
    let gate = {
        let mut gates = adopted_stop_close_gates()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gates.retain(|gate| gate.state.strong_count() > 0);
        let index = gates
            .iter()
            .position(|gate| std::ptr::eq(gate.state.as_ptr(), Arc::as_ptr(state)));
        index.map(|index| gates.remove(index))
    };
    let Some(gate) = gate else {
        return;
    };
    gate.entered.notify_one();
    gate.release.notified().await;
}

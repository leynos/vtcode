//! A test-only rendezvous inside the child launch window.
//!
//! `launch_child` publishes the queued state and then re-acquires the state
//! lock to start the run. A close that lands in that window must still refuse
//! the start, so a test parks a launch here to interleave the two
//! deterministically.

use super::*;
use std::sync::{Mutex, OnceLock, Weak};

struct LaunchCheckpoint {
    state: Weak<RwLock<ControllerState>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

fn checkpoints() -> &'static Mutex<Vec<LaunchCheckpoint>> {
    static CHECKPOINTS: OnceLock<Mutex<Vec<LaunchCheckpoint>>> = OnceLock::new();
    CHECKPOINTS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Arms a checkpoint that holds the next launch of `state` after it published
/// its queued status.
///
/// Returns the signal that the launch reached the checkpoint and the signal
/// that releases it.
pub(in crate::subagents) fn arm(state: &Arc<RwLock<ControllerState>>) -> (Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut registry = checkpoints().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|checkpoint| checkpoint.state.strong_count() > 0);
    registry.push(LaunchCheckpoint {
        state: Arc::downgrade(state),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    (entered, release)
}

/// Parks here when `state` has an armed launch checkpoint.
pub(super) async fn pass(state: &Arc<RwLock<ControllerState>>) {
    let checkpoint = {
        let mut registry = checkpoints().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.retain(|checkpoint| checkpoint.state.strong_count() > 0);
        registry
            .iter()
            .position(|checkpoint| std::ptr::eq(checkpoint.state.as_ptr(), Arc::as_ptr(state)))
            .map(|index| registry.remove(index))
    };
    let Some(checkpoint) = checkpoint else {
        return;
    };
    checkpoint.entered.notify_one();
    checkpoint.release.notified().await;
}

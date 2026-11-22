//! Test-only rendezvous points inside one attachment's critical section.
//!
//! The attachment guard is the only thing that stops two opposing edges from
//! both passing the cycle check. Parking an attachment at one of these points
//! makes the guard itself observable, so a test can prove the second
//! attachment waits rather than passing its own check.

use super::*;
use std::sync::{Mutex, OnceLock, Weak};

/// The point inside an attachment's critical section a test can park it at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AttachmentStage {
    /// Immediately after the cycle check passed: the earliest point at which
    /// the guard is held.
    AfterCycleCheck,
    /// Immediately before the link is published: after every await in the
    /// critical section.
    BeforePublication,
}

struct Checkpoint {
    state: Weak<tokio::sync::RwLock<ControllerState>>,
    stage: AttachmentStage,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

fn checkpoints() -> &'static Mutex<Vec<Checkpoint>> {
    static CHECKPOINTS: OnceLock<Mutex<Vec<Checkpoint>>> = OnceLock::new();
    CHECKPOINTS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Arms a checkpoint that holds the next attachment of `state` at `stage`.
///
/// Returns the signal that the attachment reached the checkpoint and the
/// signal that releases it.
pub(crate) fn arm(
    state: &Arc<tokio::sync::RwLock<ControllerState>>,
    stage: AttachmentStage,
) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut registry = checkpoints().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|checkpoint| checkpoint.state.strong_count() > 0);
    registry.push(Checkpoint {
        state: Arc::downgrade(state),
        stage,
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    (entered, release)
}

/// Parks here when `state` has an armed checkpoint for `stage`.
pub(super) async fn pass(state: &Arc<tokio::sync::RwLock<ControllerState>>, stage: AttachmentStage) {
    let checkpoint = {
        let mut registry = checkpoints().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.retain(|checkpoint| checkpoint.state.strong_count() > 0);
        registry
            .iter()
            .position(|checkpoint| {
                std::ptr::eq(checkpoint.state.as_ptr(), Arc::as_ptr(state)) && checkpoint.stage == stage
            })
            .map(|index| registry.remove(index))
    };
    let Some(checkpoint) = checkpoint else {
        return;
    };
    checkpoint.entered.notify_one();
    checkpoint.release.notified().await;
}

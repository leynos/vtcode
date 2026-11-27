//! Unit tests for the shared ACP session state types.
//!
//! Cancellation flags, prompt activity and lifecycle teardown are exercised in
//! sibling modules so each file stays focused.
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};

mod activity;
mod cancellation;
mod lifecycle_teardown;

/// Poll `future` once without installing a real waker.
///
/// A `Notify` waiter records itself on its first poll, so this observes
/// whether the waiter is still pending without sleeping or depending on a
/// scheduler. It records nothing about being woken, so a test that must prove
/// a wake happened uses [`recording_waker`] and [`poll_with_waker`] instead.
fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    future.as_mut().poll(&mut context)
}

/// A waker that counts how often it was woken.
///
/// A no-op waker makes "the future became ready" indistinguishable from "the
/// future was woken and then found itself ready". Counting the wakes separates
/// them: dropping the final prompt guard must wake a registered waiter, not
/// merely leave a state the waiter's next poll would accept.
#[derive(Default)]
struct WakeRecorder {
    wakes: AtomicUsize,
}

impl Wake for WakeRecorder {
    fn wake(self: Arc<Self>) {
        // The previous count is irrelevant; only that a wake happened, so the
        // returned value is deliberately discarded.
        let _ = self.wakes.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.wakes.fetch_add(1, Ordering::SeqCst);
    }
}

impl WakeRecorder {
    fn count(&self) -> usize {
        self.wakes.load(Ordering::SeqCst)
    }
}

/// Builds a waker plus the counter it feeds.
fn recording_waker() -> (Waker, Arc<WakeRecorder>) {
    let recorder = Arc::new(WakeRecorder::default());
    (Waker::from(Arc::clone(&recorder)), recorder)
}

/// Poll `future` once with an explicit waker.
fn poll_with_waker<F: Future>(mut future: Pin<&mut F>, waker: &Waker) -> Poll<F::Output> {
    let mut context = Context::from_waker(waker);
    future.as_mut().poll(&mut context)
}

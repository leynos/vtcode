//! Unit tests for the shared ACP session state types.
//!
//! Cancellation flags, prompt activity and lifecycle teardown are exercised in
//! sibling modules so each file stays focused.
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

mod activity;
mod cancellation;
mod lifecycle_teardown;

/// Poll `future` once without installing a real waker.
///
/// A `Notify` waiter records itself on its first poll, so this observes
/// whether the waiter is still pending without sleeping or depending on a
/// scheduler.
fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    future.as_mut().poll(&mut context)
}

//! Cancellation-flag bookkeeping for ACP prompts.
use super::super::SessionCancellation;
use super::{poll_with_waker, recording_waker};
use std::time::Duration;

/// A pending waiter must be woken by `cancel`, not merely find the flag set.
///
/// A spawned waiter that has not been polled yet would observe the sticky
/// cancellation bit on its first poll after `cancel`, so the test could pass
/// even if `cancel` never woke anyone. Polling a pinned future here proves the
/// waiter was registered and pending before the signal, and the recording
/// waker proves the signal reached it.
#[tokio::test]
async fn cancellation_wakes_a_pending_operation() {
    let cancellation = SessionCancellation::default();
    let (waker, recorder) = recording_waker();
    let mut waiter = std::pin::pin!(cancellation.cancelled());
    assert!(
        poll_with_waker(waiter.as_mut(), &waker).is_pending(),
        "an uncancelled signal must keep the registered waiter pending"
    );
    assert_eq!(recorder.count(), 0, "registering the waiter must not wake it");

    cancellation.cancel();
    assert_eq!(
        recorder.count(),
        1,
        "cancelling must wake the waiter that was already pending, not leave it to its next poll"
    );
    assert!(
        poll_with_waker(waiter.as_mut(), &waker).is_ready(),
        "the woken waiter must observe the cancellation"
    );
}

#[tokio::test]
async fn reset_allows_the_signal_to_be_reused_for_the_next_turn() {
    let cancellation = SessionCancellation::default();
    cancellation.cancel();
    cancellation.cancelled().await;
    assert!(cancellation.reset());

    assert!(
        tokio::time::timeout(Duration::from_millis(10), cancellation.cancelled())
            .await
            .is_err(),
        "reset signal must wait for a fresh cancellation"
    );
}

#[test]
fn close_cancellation_cannot_be_reset_for_another_prompt() {
    let cancellation = SessionCancellation::default();
    cancellation.close();

    assert!(!cancellation.reset());
    assert!(cancellation.is_cancelled());
    assert!(cancellation.is_closed());
}

/// Reference model for [`SessionCancellation`]'s two sticky flags.
///
/// `cancel` and `close` are idempotent setters, and `reset` clears only
/// `cancelled` — and only while the session has not been closed. The
/// generated sequences are sequential; they do not prove anything about
/// concurrent interleavings.
#[derive(Debug, Default)]
struct CancellationModel {
    cancelled: bool,
    closed: bool,
}

impl CancellationModel {
    fn cancel(&mut self) {
        self.cancelled = true;
    }

    fn close(&mut self) {
        self.cancelled = true;
        self.closed = true;
    }

    fn reset(&mut self) -> bool {
        if self.closed {
            return false;
        }
        self.cancelled = false;
        true
    }
}

#[derive(Clone, Copy, Debug)]
enum CancellationOp {
    Cancel,
    Close,
    Reset,
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig {
        cases: 128,
        max_shrink_iters: 256,
        ..proptest::prelude::ProptestConfig::default()
    })]

    #[test]
    fn cancellation_flags_match_the_reference_model(
        ops in proptest::collection::vec(
            proptest::prop_oneof![
                proptest::prelude::Just(CancellationOp::Cancel),
                proptest::prelude::Just(CancellationOp::Close),
                proptest::prelude::Just(CancellationOp::Reset),
            ],
            0..12,
        ),
    ) {
        let cancellation = SessionCancellation::default();
        let mut model = CancellationModel::default();

        for op in ops {
            match op {
                CancellationOp::Cancel => {
                    cancellation.cancel();
                    model.cancel();
                }
                CancellationOp::Close => {
                    cancellation.close();
                    model.close();
                }
                CancellationOp::Reset => {
                    proptest::prop_assert_eq!(
                        cancellation.reset(),
                        model.reset(),
                        "reset must report whether the signal was revived"
                    );
                }
            }

            proptest::prop_assert_eq!(cancellation.is_cancelled(), model.cancelled);
            proptest::prop_assert_eq!(cancellation.is_closed(), model.closed);
        }

        // Closing is terminal: no later operation may revive the session.
        if model.closed {
            proptest::prop_assert!(cancellation.is_closed());
            proptest::prop_assert!(!cancellation.reset());
            proptest::prop_assert!(cancellation.is_cancelled());
        }
    }
}

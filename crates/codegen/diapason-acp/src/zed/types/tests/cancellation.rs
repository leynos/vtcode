//! Cancellation-flag bookkeeping for ACP prompts.
use super::super::SessionCancellation;
use std::time::Duration;

#[tokio::test]
async fn cancellation_wakes_a_pending_operation() {
    let cancellation = SessionCancellation::default();
    let waiter = cancellation.clone();
    let task = tokio::spawn(async move { waiter.cancelled().await });

    tokio::task::yield_now().await;
    cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("cancellation waiter should wake")
        .expect("waiter task should finish");
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

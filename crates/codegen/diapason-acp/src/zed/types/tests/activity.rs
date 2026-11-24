//! Prompt-activity bookkeeping and the close handshake.
use super::super::{SessionActivity, SessionPromptGuard};
use super::poll_once;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Reference model for [`SessionActivity`] and [`SessionPromptGuard`].
///
/// A guard exists exactly while its prompt is in flight: starting one
/// increments the in-flight count, and dropping the guard decrements it
/// exactly once. Closing is sticky — once `begin_close` runs no further
/// prompt may start, and guards taken before the close stay valid so the
/// close can wait for them. `wait_for_prompts` completes if and only if the
/// in-flight count is zero.
///
/// The generated sequences are sequential. They pin the bookkeeping a close
/// depends on; they say nothing about concurrent interleavings.
#[derive(Debug, Default)]
struct ActivityModel {
    in_flight: usize,
    closing: bool,
}

impl ActivityModel {
    /// Mirrors the double-check in `try_start_prompt`: a close that lands
    /// between the two checks must not leave a prompt counted.
    fn try_start_prompt(&mut self) -> bool {
        if self.closing {
            return false;
        }
        self.in_flight += 1;
        true
    }

    fn drop_guard(&mut self) {
        assert!(self.in_flight > 0, "reference model underflow");
        self.in_flight -= 1;
    }

    fn begin_close(&mut self) {
        self.closing = true;
    }
}

#[derive(Clone, Copy, Debug)]
enum ActivityOp {
    StartPrompt,
    DropGuard,
    Close,
    WaitForPrompts,
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig {
        cases: 64,
        max_shrink_iters: 256,
        ..proptest::prelude::ProptestConfig::default()
    })]

    #[test]
    fn session_activity_matches_the_reference_model(
        ops in proptest::collection::vec(
            proptest::prop_oneof![
                proptest::prelude::Just(ActivityOp::StartPrompt),
                proptest::prelude::Just(ActivityOp::DropGuard),
                proptest::prelude::Just(ActivityOp::Close),
                proptest::prelude::Just(ActivityOp::WaitForPrompts),
            ],
            0..16,
        ),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("current-thread runtime");
        let _guard = runtime.enter();

        let activity = SessionActivity::default();
        let mut model = ActivityModel::default();
        let mut guards: Vec<SessionPromptGuard> = Vec::new();

        for op in ops {
            match op {
                ActivityOp::StartPrompt => {
                    let guard = activity.try_start_prompt();
                    let model_grants = model.try_start_prompt();
                    proptest::prop_assert_eq!(
                        guard.is_some(),
                        model_grants,
                        "prompt admission must match the model"
                    );
                    match guard {
                        Some(guard) => {
                            proptest::prop_assert_eq!(
                                guard.is_closing(),
                                model.closing,
                                "a guard must report the close state it was taken under"
                            );
                            guards.push(guard);
                        }
                        None => {
                            proptest::prop_assert!(
                                model.closing,
                                "a refused prompt must be explained by a pending close"
                            );
                        }
                    }
                }
                ActivityOp::DropGuard => {
                    if let Some(guard) = guards.pop() {
                        drop(guard);
                        model.drop_guard();
                    }
                }
                ActivityOp::Close => {
                    activity.begin_close();
                    model.begin_close();
                }
                ActivityOp::WaitForPrompts => {
                    let idle = runtime.block_on(tokio::time::timeout(
                        Duration::from_millis(50),
                        activity.wait_for_prompts(),
                    ));
                    proptest::prop_assert_eq!(
                        idle.is_ok(),
                        model.in_flight == 0,
                        "waiting for prompts must complete exactly when none are in flight"
                    );
                }
            }

            // A pending close is sticky for the rest of the sequence. The
            // refused branch of `try_start_prompt` does not mutate the
            // count, so this observation cannot perturb the model.
            if model.closing {
                proptest::prop_assert!(
                    activity.try_start_prompt().is_none(),
                    "a closing session must refuse every later prompt"
                );
            }

            proptest::prop_assert_eq!(
                activity.active_prompts.load(Ordering::Acquire),
                model.in_flight,
                "the in-flight count must match the model after {:?}",
                op
            );
        }

        // Closing is sticky and does not invalidate guards taken earlier,
        // so one waiter registered before the drain must stay pending
        // across every intermediate departure and then complete. Reusing
        // the same waiter matters: a fresh waiter created after the final
        // drop would succeed from the count alone and would not detect a
        // missing `prompts_idle.notify_waiters()`.
        activity.begin_close();
        let outstanding = guards.len();
        proptest::prop_assert_eq!(outstanding, model.in_flight);

        let mut waiter = std::pin::pin!(activity.wait_for_prompts());
        if outstanding > 0 {
            proptest::prop_assert!(
                poll_once(waiter.as_mut()).is_pending(),
                "an in-flight prompt must keep the close waiting"
            );
        }

        let mut remaining = outstanding;
        for guard in guards.drain(..) {
            drop(guard);
            model.drop_guard();
            remaining -= 1;
            proptest::prop_assert_eq!(
                activity.active_prompts.load(Ordering::Acquire),
                model.in_flight,
                "the in-flight count must match the model mid-drain"
            );
            if remaining > 0 {
                proptest::prop_assert!(
                    poll_once(waiter.as_mut()).is_pending(),
                    "a remaining prompt must keep the registered waiter pending"
                );
            }
        }
        proptest::prop_assert_eq!(remaining, 0, "the drain must visit every guard once");
        proptest::prop_assert!(
            poll_once(waiter.as_mut()).is_ready(),
            "dropping every guard must let the registered waiter finish"
        );
    }
}

/// A waiter registered while prompts are in flight must stay pending across
/// every intermediate departure and complete only when the last guard drops.
///
/// Polling one waiter throughout is what makes this a regression test for
/// the final-guard notification: a waiter created after the last drop would
/// return from the count check without ever waiting.
#[tokio::test]
async fn final_guard_drop_wakes_the_waiter_that_was_already_registered() {
    let activity = SessionActivity::default();
    let first = activity.try_start_prompt().expect("an open session admits a prompt");
    let second = activity.try_start_prompt().expect("an open session admits a second prompt");

    let mut waiter = std::pin::pin!(activity.wait_for_prompts());
    assert!(poll_once(waiter.as_mut()).is_pending(), "two in-flight prompts must keep the close waiting");

    drop(first);
    assert_eq!(activity.active_prompts.load(Ordering::Acquire), 1);
    assert!(
        poll_once(waiter.as_mut()).is_pending(),
        "one remaining prompt must keep the registered waiter pending"
    );

    drop(second);
    assert_eq!(activity.active_prompts.load(Ordering::Acquire), 0);
    assert!(poll_once(waiter.as_mut()).is_ready(), "the final guard drop must wake the registered waiter");
}

/// A guard taken before `begin_close` stays valid and observes the sticky
/// close; its departure still releases a waiter registered afterwards.
#[tokio::test]
async fn guards_taken_before_a_close_observe_it_and_still_release_the_waiter() {
    let activity = SessionActivity::default();
    let guard = activity.try_start_prompt().expect("an open session admits a prompt");
    assert!(!guard.is_closing(), "an open session must not report a close");

    activity.begin_close();
    assert!(guard.is_closing(), "a live guard must observe the sticky close");
    assert!(activity.try_start_prompt().is_none(), "a closing session must refuse new prompts");

    let mut waiter = std::pin::pin!(activity.wait_for_prompts());
    assert!(poll_once(waiter.as_mut()).is_pending(), "the live guard must keep the close waiting");

    drop(guard);
    assert!(poll_once(waiter.as_mut()).is_ready(), "the live guard's departure must release the waiter");
    assert!(activity.try_start_prompt().is_none(), "closing stays terminal after every prompt departs");
}

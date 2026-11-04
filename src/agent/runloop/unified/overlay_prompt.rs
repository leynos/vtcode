use std::sync::Arc;

use anyhow::Result;
use diapason_core::core::interfaces::ui::UiSession;
use diapason_ui::tui::app::{InlineEvent, InlineHandle, TransientEvent, TransientRequest, TransientSubmission};
use tokio::sync::Notify;
use tokio::task;

use super::state::CtrlCState;

pub(crate) enum OverlayWaitOutcome<T> {
    Submitted(T),
    Cancelled,
    Interrupted,
    /// A bridge prompt arrived while a modal owned the input surface. The
    /// prompt was requeued and must be handled after the modal closes.
    Deferred,
    Exit,
}

pub(crate) async fn show_overlay_and_wait<S, T, F>(
    handle: &InlineHandle,
    session: &mut S,
    request: TransientRequest,
    ctrl_c_state: &Arc<CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
    map_submission: F,
) -> Result<OverlayWaitOutcome<T>>
where
    S: UiSession + ?Sized,
    F: FnMut(TransientSubmission) -> Option<T>,
{
    tracing::info!(
        target: "diapason.planning_workflow",
        "show_overlay_and_wait: showing transient request"
    );
    handle.show_transient(request);
    handle.force_redraw();
    task::yield_now().await;
    let result = wait_for_overlay_submission(handle, session, ctrl_c_state, ctrl_c_notify, map_submission).await;
    tracing::info!(
        target: "diapason.planning_workflow",
        overlay_result = "completed",
        "show_overlay_and_wait: completed"
    );
    result
}

pub(crate) async fn wait_for_overlay_submission<S, T, F>(
    handle: &InlineHandle,
    session: &mut S,
    ctrl_c_state: &Arc<CtrlCState>,
    ctrl_c_notify: &Arc<Notify>,
    mut map_submission: F,
) -> Result<OverlayWaitOutcome<T>>
where
    S: UiSession + ?Sized,
    F: FnMut(TransientSubmission) -> Option<T>,
{
    loop {
        if ctrl_c_state.is_cancel_requested() {
            close_overlay(handle).await;
            return Ok(OverlayWaitOutcome::Interrupted);
        }

        let notify = ctrl_c_notify.clone();
        tracing::info!(
            target: "diapason.planning_workflow",
            "wait_for_overlay_submission: waiting for event"
        );
        let maybe_event = tokio::select! {
            _ = notify.notified() => None,
            event = session.next_event() => event,
        };
        tracing::info!(
            target: "diapason.planning_workflow",
            event_received = true,
            "wait_for_overlay_submission: event received"
        );

        let Some(event) = maybe_event else {
            close_overlay(handle).await;
            if ctrl_c_state.is_cancel_requested() {
                return Ok(OverlayWaitOutcome::Interrupted);
            }
            return Ok(OverlayWaitOutcome::Exit);
        };

        match event {
            InlineEvent::Interrupt => {
                tracing::info!(
                    target: "diapason.planning_workflow",
                    "wait_for_overlay_submission: interrupt event"
                );
                crate::agent::runloop::unified::stop_requests::request_local_stop(ctrl_c_state, ctrl_c_notify);
                close_overlay(handle).await;
                return Ok(OverlayWaitOutcome::Interrupted);
            }
            InlineEvent::Transient(TransientEvent::Submitted(submission)) => {
                tracing::info!(
                    target: "diapason.planning_workflow",
                    submission = "received",
                    "wait_for_overlay_submission: submitted event"
                );
                ctrl_c_state.reset();
                if let Some(mapped) = map_submission(submission) {
                    close_overlay(handle).await;
                    return Ok(OverlayWaitOutcome::Submitted(mapped));
                }
            }
            InlineEvent::Transient(TransientEvent::Cancelled) | InlineEvent::Cancel => {
                tracing::info!(
                    target: "diapason.planning_workflow",
                    "wait_for_overlay_submission: cancelled event"
                );
                ctrl_c_state.reset();
                close_overlay(handle).await;
                return Ok(OverlayWaitOutcome::Cancelled);
            }
            InlineEvent::Exit => {
                tracing::info!(
                    target: "diapason.planning_workflow",
                    "wait_for_overlay_submission: exit event"
                );
                ctrl_c_state.reset();
                close_overlay(handle).await;
                return Ok(OverlayWaitOutcome::Exit);
            }
            InlineEvent::WebmcpSubmit(input) => {
                let deferral = session.defer_event(InlineEvent::WebmcpSubmit(input));
                close_overlay(handle).await;
                deferral?;
                return Ok(OverlayWaitOutcome::Deferred);
            }
            InlineEvent::Submit(_) | InlineEvent::QueueSubmit(_) => continue,
            InlineEvent::Transient(_) => {}
            _ => {
                tracing::info!(
                    target: "diapason.planning_workflow",
                    event_type = "other",
                    "wait_for_overlay_submission: other event"
                );
            }
        }
    }
}

async fn close_overlay(handle: &InlineHandle) {
    handle.close_transient();
    handle.force_redraw();
    task::yield_now().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use diapason_core::core::interfaces::ui::UiSession;
    use diapason_ui::tui::app::{InlineCommand, InlineListItem};
    use tokio::sync::{Notify, mpsc};

    struct FailingDeferralSession {
        handle: InlineHandle,
        event: Option<InlineEvent>,
    }

    #[async_trait]
    impl UiSession for FailingDeferralSession {
        fn inline_handle(&self) -> &InlineHandle {
            &self.handle
        }

        fn defer_event(&self, _event: InlineEvent) -> Result<()> {
            Err(anyhow::anyhow!("deferred input queue is full"))
        }

        async fn next_event(&mut self) -> Option<InlineEvent> {
            self.event.take()
        }
    }

    #[tokio::test]
    async fn failed_webmcp_deferral_still_closes_the_overlay() {
        let (command_sender, mut command_receiver) = mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(command_sender);
        handle.show_list_modal("test".into(), Vec::new(), Vec::<InlineListItem>::new(), None, None);
        let mut session = FailingDeferralSession {
            handle: handle.clone(),
            event: Some(InlineEvent::WebmcpSubmit("/exit".into())),
        };
        let ctrl_c_state = Arc::new(CtrlCState::new());
        let ctrl_c_notify = Arc::new(Notify::new());

        let result =
            wait_for_overlay_submission(&handle, &mut session, &ctrl_c_state, &ctrl_c_notify, |_| None::<()>).await;

        assert!(result.is_err());
        assert!(matches!(command_receiver.recv().await, Some(InlineCommand::ShowTransient { .. })));
        assert!(matches!(command_receiver.recv().await, Some(InlineCommand::CloseTransient)));
        assert!(matches!(command_receiver.recv().await, Some(InlineCommand::ForceRedraw)));
    }
}

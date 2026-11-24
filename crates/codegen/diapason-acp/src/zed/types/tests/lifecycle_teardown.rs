//! Teardown of the ACP task lifecycle forwarder.
use super::super::stop_task_lifecycle_tasks;
use tokio::sync::oneshot;

/// A publisher that panics must not stop teardown before the forwarder is
/// awaited.
///
/// Returning on the publisher's join error left the aborted forwarder
/// detached: its task was never driven to cancellation, so it could outlive
/// the session. Teardown must save the publisher's error, await the
/// forwarder too, and only then return the saved error.
#[tokio::test]
async fn a_panicking_publisher_does_not_skip_awaiting_the_forwarder() {
    /// Signals that the forwarder's future was dropped, which aborting a
    /// task and awaiting its handle guarantees.
    struct SignalOnDrop(Option<oneshot::Sender<()>>);

    impl Drop for SignalOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                // A closed receiver is the expected end of the test; the
                // signal has done its work either way, so both outcomes are
                // consumed deliberately.
                match sender.send(()) {
                    Ok(()) | Err(()) => {}
                }
            }
        }
    }

    let (forwarder_dropped, mut dropped) = oneshot::channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let forwarder = tokio::spawn(async move {
        let _signal = SignalOnDrop(Some(forwarder_dropped));
        // The drop signal only exists once this future has been polled.
        // Teardown must not start before then: an abort that lands first
        // drops the future without ever creating the signal, and the test
        // would report a missing signal it never armed rather than the
        // teardown defect it is checking.
        // The receiver is either waiting or already gone; the signal has
        // done its work either way, so both outcomes are consumed.
        match ready_tx.send(()) {
            Ok(()) | Err(()) => {}
        }
        std::future::pending::<()>().await;
    });
    ready_rx
        .await
        .expect("the forwarder must install its drop signal before teardown starts");
    let publisher = tokio::spawn(async {
        panic!("lifecycle publisher failed");
    });
    // Let the publisher settle with its panic before teardown starts;
    // otherwise the abort could cancel it and mask the failure.
    while !publisher.is_finished() {
        tokio::task::yield_now().await;
    }

    let error = stop_task_lifecycle_tasks(Some(forwarder), Some(publisher))
        .await
        .expect_err("the publisher's panic must surface as a teardown error");

    assert!(
        format!("{error:#}").contains("snapshot publisher"),
        "the saved error must name the publisher: {error:#}"
    );
    assert!(dropped.try_recv().is_ok(), "the forwarder must be awaited even after the publisher fails");
}

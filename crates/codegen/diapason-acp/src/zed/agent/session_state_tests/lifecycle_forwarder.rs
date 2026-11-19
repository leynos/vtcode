//! Shutdown behaviour for the ACP task lifecycle forwarder.
use super::*;
use assert_fs::TempDir;

/// The initial-snapshot publisher writes into the forwarder's queue, so it
/// must stop with the forwarder.
///
/// A publisher left running outlives the receiver and keeps enqueueing into a
/// queue nothing drains, leaking a task for every attach/stop cycle. Storing
/// the handle and aborting it here is what bounds that pair.
#[tokio::test]
async fn stopping_the_forwarder_stops_its_initial_snapshot_publisher() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();

    let publisher = {
        let mut data = session.data.lock().unwrap();
        data.task_lifecycle_forwarder = Some(tokio::spawn(std::future::pending::<()>()));
        let publisher = tokio::spawn(std::future::pending::<()>());
        let abort_handle = publisher.abort_handle();
        data.task_lifecycle_snapshot_publisher = Some(publisher);
        abort_handle
    };

    session
        .stop_task_lifecycle_forwarder()
        .await
        .expect("stop the lifecycle forwarder");

    assert!(publisher.is_finished(), "the initial-snapshot publisher must be aborted with its forwarder");
    let data = session.data.lock().unwrap();
    assert!(data.task_lifecycle_forwarder.is_none(), "the forwarder handle must be cleared");
    assert!(
        data.task_lifecycle_snapshot_publisher.is_none(),
        "the publisher handle must be cleared once stopped"
    );
}

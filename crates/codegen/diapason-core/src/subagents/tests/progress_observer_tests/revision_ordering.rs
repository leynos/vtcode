//! Lifecycle-revision ordering for captures published through one clock.
//!
//! Declared by `progress_observer_tests.rs`, which keeps the observer fixtures
//! and the routing cases. These cases pin that a revision is assigned under the
//! state lock that captured the entry, so a capture delivered out of order
//! cannot overwrite a newer state.
use super::*;
use chrono::Utc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Records each publication with the lifecycle revision that ordered it.
#[derive(Default)]
struct RevisionRecordingObserver {
    children: Mutex<Vec<(SubagentStatusEntry, u64)>>,
    background: Mutex<Vec<(BackgroundSubprocessEntry, u64)>>,
}

impl SubagentProgressObserver for RevisionRecordingObserver {
    fn subagent_status_changed(&self, entry: SubagentStatusEntry) {
        self.subagent_status_published(entry, 0);
    }

    fn background_status_changed(&self, entry: BackgroundSubprocessEntry) {
        self.background_status_published(entry, 0);
    }

    fn subagent_status_published(&self, entry: SubagentStatusEntry, lifecycle_revision: u64) {
        self.children.lock().expect("children lock").push((entry, lifecycle_revision));
    }

    fn background_status_published(&self, entry: BackgroundSubprocessEntry, lifecycle_revision: u64) {
        self.background
            .lock()
            .expect("background lock")
            .push((entry, lifecycle_revision));
    }
}

/// Parks the first `Running` background notification until it is released.
///
/// A publication that is delivered out of order cannot be produced by
/// awaiting one publish and then the other, so the observer holds the older
/// capture on its delivering thread until the later capture has been
/// delivered by another worker.
struct OutOfOrderDeliveryObserver {
    background: Mutex<Vec<(BackgroundSubprocessEntry, u64)>>,
    captured_running: std::sync::mpsc::Sender<()>,
    release_running: Mutex<std::sync::mpsc::Receiver<()>>,
    park_running: AtomicBool,
}

impl SubagentProgressObserver for OutOfOrderDeliveryObserver {
    fn subagent_status_changed(&self, _entry: SubagentStatusEntry) {}

    fn background_status_changed(&self, entry: BackgroundSubprocessEntry) {
        self.background_status_published(entry, 0);
    }

    fn background_status_published(&self, entry: BackgroundSubprocessEntry, lifecycle_revision: u64) {
        if entry.status == BackgroundSubprocessStatus::Running && self.park_running.swap(false, Ordering::SeqCst) {
            // A closed channel means the test already finished; the release
            // wait below then times out rather than hanging.
            let _announced = self.captured_running.send(());
            let receiver = self.release_running.lock().expect("release lock");
            // A bounded wait keeps a scheduling fault visible as an assertion
            // failure instead of a test that hangs.
            let _released = receiver.recv_timeout(Duration::from_secs(10));
        }
        self.background
            .lock()
            .expect("background lock")
            .push((entry, lifecycle_revision));
    }
}

/// A capture taken before a state change keeps the lower revision, so a
/// delayed delivery of it cannot overwrite the newer state.
///
/// Reversing the publication order is what matters: when the revision was
/// allocated as the adapter enqueued the item, the older capture delivered
/// last took the higher revision and won. Assigning it under the state lock
/// that captured the entry makes revision order and capture order the same
/// total order, whatever order the deliveries arrive in.
#[tokio::test]
async fn a_child_capture_taken_before_a_transition_keeps_the_lower_revision() {
    let (_temp, controller) = progress_controller().await;
    let observer = Arc::new(RevisionRecordingObserver::default());
    let _registration = controller.register_progress_observer("parent-session", observer.clone());
    seed_child_records(&controller).await;

    controller.publish_child_status("child-1").await;
    {
        let mut state = controller.state.write().await;
        let record = state.children.get_mut("child-1").expect("seeded child record");
        record.status = SubagentStatus::Completed;
        record.completed_at = Some(Utc::now());
    }
    controller.publish_child_status("child-1").await;

    let observed = observer.children.lock().expect("children lock").clone();
    assert_eq!(observed.len(), 2, "each capture must publish once");
    let (older, older_revision) = &observed[0];
    let (newer, newer_revision) = &observed[1];
    assert_eq!(older.status, SubagentStatus::Running, "the first capture must observe the running state");
    assert_eq!(newer.status, SubagentStatus::Completed, "the second capture must observe the later state");
    assert!(
        older_revision < newer_revision,
        "the capture taken first must keep the lower revision, got {older_revision} then {newer_revision}"
    );
}

/// The background path stamps under the same lock and keeps the same order.
#[tokio::test]
async fn a_background_capture_taken_before_a_transition_keeps_the_lower_revision() {
    let (_temp, controller) = progress_controller().await;
    let observer = Arc::new(RevisionRecordingObserver::default());
    let _registration = controller.register_progress_observer("parent-session", observer.clone());
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(
            "background-1".to_string(),
            background_record("background-1", "owner-a", "parent-session", Utc::now()),
        );
    }

    controller.publish_background_status("background-1").await;
    {
        let mut state = controller.state.write().await;
        let record = state
            .background_children
            .get_mut("background-1")
            .expect("seeded background record");
        record.status = BackgroundSubprocessStatus::Stopped;
        record.ended_at = Some(Utc::now());
    }
    controller.publish_background_status("background-1").await;

    let observed = observer.background.lock().expect("background lock").clone();
    assert_eq!(observed.len(), 2, "each capture must publish once");
    let (older, older_revision) = &observed[0];
    let (newer, newer_revision) = &observed[1];
    assert_eq!(older.status, BackgroundSubprocessStatus::Running);
    assert_eq!(newer.status, BackgroundSubprocessStatus::Stopped);
    assert!(
        older_revision < newer_revision,
        "the capture taken first must keep the lower revision, got {older_revision} then {newer_revision}"
    );
}

/// The snapshot pass's base revision and live captures share one clock.
///
/// The adapter reserves the base before it registers its observer, so a
/// snapshot taken from the same state a transition reports must not tie with
/// that transition: every live capture has to carry a strictly higher
/// revision or the forwarder would drop one of the two.
#[tokio::test]
async fn a_reserved_snapshot_base_is_older_than_every_later_capture() {
    let (_temp, controller) = progress_controller().await;
    let observer = Arc::new(RevisionRecordingObserver::default());
    let snapshot_revision = controller.reserve_lifecycle_revision();
    let _registration = controller.register_progress_observer("parent-session", observer.clone());
    seed_child_records(&controller).await;

    controller.publish_child_status("child-1").await;

    let observed = observer.children.lock().expect("children lock").clone();
    let (_, live_revision) = observed.first().expect("the live capture must be published");
    assert!(
        *live_revision > snapshot_revision,
        "a live capture must outrank the reserved snapshot base, got {live_revision} against {snapshot_revision}"
    );
}

/// A stale capture handed to `publish_subagent_progress` must not be published.
///
/// The caller builds its entry under its own lock and only later awaits the
/// publish, so the record can have moved on in that gap. Publishing the
/// caller's capture would give the older state the newer revision and let it
/// overwrite the newer one in a client's status list.
#[tokio::test]
async fn a_stale_child_capture_is_replaced_by_the_current_record() {
    let (_temp, controller) = progress_controller().await;
    let observer = Arc::new(RevisionRecordingObserver::default());
    let _registration = controller.register_progress_observer("parent-session", observer.clone());
    seed_child_records(&controller).await;

    controller.publish_child_status("child-1").await;
    // Retain the capture the first publication delivered, as a caller that
    // built its entry before a later transition would.
    let stale = observer
        .children
        .lock()
        .expect("children lock")
        .first()
        .map(|(entry, _)| entry.clone())
        .expect("the running capture must be published");
    {
        let mut state = controller.state.write().await;
        let record = state.children.get_mut("child-1").expect("seeded child record");
        record.status = SubagentStatus::Completed;
        record.completed_at = Some(Utc::now());
    }
    controller.publish_child_status("child-1").await;

    // The caller publishes the capture it retained before the transition.
    controller.publish_subagent_progress(stale).await;

    let observed = observer.children.lock().expect("children lock").clone();
    assert_eq!(observed.len(), 3, "each publication must report once");
    let completed_revision = observed
        .iter()
        .find_map(|(entry, revision)| (entry.status == SubagentStatus::Completed).then_some(*revision))
        .expect("the completed transition must be published");
    assert!(
        observed
            .iter()
            .all(|(entry, revision)| entry.status != SubagentStatus::Running || *revision < completed_revision),
        "a running capture must not outrank the completed transition: {observed:?}"
    );
    assert_eq!(
        observed.last().expect("the stale publication").0.status,
        SubagentStatus::Completed,
        "publishing a stale capture must report the state that is current now"
    );
}

/// A capture delivered last must still carry the lower revision.
///
/// Sequential publishes cannot detect delivery-order stamping: the first
/// publish finishes before the second begins, so any scheme that stamps at
/// publication time looks correct. Here the observer parks the `Running`
/// capture while another worker publishes `Stopped`, so the older snapshot is
/// delivered last and only a revision assigned at capture time keeps it lower.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_background_capture_delivered_last_keeps_the_lower_revision() {
    let (_temp, controller) = progress_controller().await;
    let (captured_tx, captured_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let observer = Arc::new(OutOfOrderDeliveryObserver {
        background: Mutex::new(Vec::new()),
        captured_running: captured_tx,
        release_running: Mutex::new(release_rx),
        park_running: AtomicBool::new(true),
    });
    let _registration = controller.register_progress_observer("parent-session", observer.clone());
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(
            "background-1".to_string(),
            background_record("background-1", "owner-a", "parent-session", Utc::now()),
        );
    }
    let controller = Arc::new(controller);

    let first = {
        let controller = Arc::clone(&controller);
        tokio::spawn(async move { controller.publish_background_status("background-1").await })
    };
    // The parked publish has captured its entry and revision, so the state
    // transition below cannot be observed by it.
    tokio::task::spawn_blocking(move || captured_rx.recv_timeout(Duration::from_secs(10)))
        .await
        .expect("join the capture wait")
        .expect("the running capture must reach the observer before it parks");
    {
        let mut state = controller.state.write().await;
        let record = state
            .background_children
            .get_mut("background-1")
            .expect("seeded background record");
        record.status = BackgroundSubprocessStatus::Stopped;
        record.ended_at = Some(Utc::now());
    }
    controller.publish_background_status("background-1").await;
    drop(release_tx);
    first.await.expect("join the parked publication");

    let observed = observer.background.lock().expect("background lock").clone();
    assert_eq!(observed.len(), 2, "each capture must publish once");
    let (oldest, oldest_revision) = &observed[0];
    let (latest, latest_revision) = &observed[1];
    assert_eq!(
        oldest.status,
        BackgroundSubprocessStatus::Stopped,
        "the later capture must be delivered first: {observed:?}"
    );
    assert_eq!(latest.status, BackgroundSubprocessStatus::Running, "the parked capture must be delivered last");
    assert!(
        latest_revision < oldest_revision,
        "a capture delivered last must keep the lower revision, got {latest_revision} after {oldest_revision}"
    );
}

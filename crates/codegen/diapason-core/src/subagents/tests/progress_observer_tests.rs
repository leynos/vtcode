//! Checks that subagent and background-process progress notifications stay scoped to their parent session.
use super::*;

#[derive(Default)]
struct RecordingObserver {
    children: Mutex<Vec<SubagentStatusEntry>>,
    background: Mutex<Vec<BackgroundSubprocessEntry>>,
}

impl SubagentProgressObserver for RecordingObserver {
    fn subagent_status_changed(&self, entry: SubagentStatusEntry) {
        self.children.lock().expect("children lock").push(entry);
    }

    fn background_status_changed(&self, entry: BackgroundSubprocessEntry) {
        self.background.lock().expect("background lock").push(entry);
    }
}

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

async fn progress_controller() -> (TempDir, SubagentController) {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");
    (temp, controller)
}

fn register_session_observers(
    controller: &SubagentController,
) -> (
    Arc<RecordingObserver>,
    Arc<RecordingObserver>,
    SubagentProgressObserverRegistration,
    SubagentProgressObserverRegistration,
) {
    let parent_observer = Arc::new(RecordingObserver::default());
    let other_observer = Arc::new(RecordingObserver::default());
    let parent_registration = controller.register_progress_observer("parent-session", parent_observer.clone());
    let other_registration = controller.register_progress_observer("other-session", other_observer.clone());
    (parent_observer, other_observer, parent_registration, other_registration)
}

async fn seed_child_records(controller: &SubagentController) {
    let spec = read_only_test_spec("progress-child");
    let mut state = controller.state.write().await;
    state.children.insert(
        "child-1".to_string(),
        test_child_record("child-1", "child-1-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
    );
    state.children.insert(
        "child-2".to_string(),
        test_child_record("child-2", "child-2-session", "other-session", &spec, SubagentStatus::Running, 1, None),
    );
}

fn background_record(
    id: &str,
    owner_session_id: &str,
    parent_session_id: &str,
    timestamp: chrono::DateTime<Utc>,
) -> BackgroundRecord {
    BackgroundRecord {
        id: id.to_string(),
        owner_session_id: Some(owner_session_id.to_string()),
        agent_name: "watcher".to_string(),
        display_label: "Watcher".to_string(),
        description: "Watch the workspace".to_string(),
        source: "workspace".to_string(),
        colour: None,
        session_id: "background-session".to_string(),
        parent_session_id: parent_session_id.to_string(),
        exec_session_id: format!("exec-{id}"),
        desired_enabled: true,
        status: BackgroundSubprocessStatus::Running,
        created_at: timestamp,
        updated_at: timestamp,
        started_at: Some(timestamp),
        ended_at: None,
        pid: Some(42),
        prompt: "Watch the workspace".to_string(),
        summary: Some("running".to_string()),
        error: None,
        archive_path: None,
        transcript_path: None,
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        restart_attempts: 0,
    }
}

#[tokio::test]
async fn child_status_progress_is_scoped_to_parent_session() {
    let (_temp, controller) = progress_controller().await;
    let (parent_observer, other_observer, _parent_registration, _other_registration) =
        register_session_observers(&controller);
    seed_child_records(&controller).await;

    controller.signal_shutdown().await;

    let parent_children = parent_observer.children.lock().expect("children lock").clone();
    let other_children = other_observer.children.lock().expect("children lock").clone();
    let parent_update = parent_children.last().expect("parent child update");
    let parent_ids = parent_children.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>();
    let other_ids = other_children.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>();
    assert_eq!(parent_update.status, SubagentStatus::Closed);
    assert_eq!(parent_ids, ["child-1"]);
    assert_eq!(other_ids, ["child-2"]);
}

#[tokio::test]
async fn background_status_progress_is_scoped_to_parent_session() {
    let (_temp, controller) = progress_controller().await;
    let (parent_observer, other_observer, _parent_registration, _other_registration) =
        register_session_observers(&controller);
    let timestamp = Utc::now();
    {
        let mut state = controller.state.write().await;
        // The owner is deliberately distinct from the routing session: an
        // implementation that keyed observation on the owner instead of the
        // parent session would reach no registered observer and this test
        // would fail instead of silently routing.
        state.background_children.insert(
            "background-1".to_string(),
            background_record("background-1", "owner-a", "parent-session", timestamp),
        );
        state.background_children.insert(
            "background-2".to_string(),
            background_record("background-2", "owner-b", "other-session", timestamp),
        );
    }

    controller.publish_background_status("background-1").await;
    controller.publish_background_status("background-2").await;

    let parent_background = parent_observer.background.lock().expect("background lock").clone();
    let other_background = other_observer.background.lock().expect("background lock").clone();
    let parent_update = parent_background.last().expect("parent background update");
    let parent_ids = parent_background.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>();
    let other_ids = other_background.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>();
    assert_eq!(parent_update.status, BackgroundSubprocessStatus::Running);
    assert_eq!(parent_ids, ["background-1"]);
    assert_eq!(other_ids, ["background-2"]);
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

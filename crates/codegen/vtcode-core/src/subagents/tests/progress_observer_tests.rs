//! Checks that subagent and background-process progress notifications stay scoped to their parent session.
use super::*;

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

async fn progress_controller() -> (TempDir, SubagentController) {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default()))
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

fn background_record(id: &str, parent_session_id: &str, timestamp: chrono::DateTime<Utc>) -> BackgroundRecord {
    BackgroundRecord {
        id: id.to_string(),
        owner_session_id: Some(parent_session_id.to_string()),
        agent_name: "watcher".to_string(),
        display_label: "Watcher".to_string(),
        description: "Watch the workspace".to_string(),
        source: "workspace".to_string(),
        color: None,
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
        state
            .background_children
            .insert("background-1".to_string(), background_record("background-1", "parent-session", timestamp));
        state
            .background_children
            .insert("background-2".to_string(), background_record("background-2", "other-session", timestamp));
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

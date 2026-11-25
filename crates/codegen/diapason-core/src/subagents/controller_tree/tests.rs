//! Regressions for graft routing and observer publication.
//!
//! The lifecycle clock handoff has its own module, so these cases concentrate
//! on which tree a grafted controller publishes through.

use super::*;
use crate::config::DiapasonConfig;
use crate::config::types::ReasoningEffortLevel;
use crate::subagents::BackgroundSubprocessEntry;
use crate::subagents::ChildRecord;
use crate::subagents::SubagentControllerConfig;
use crate::subagents::SubagentProgressObserver;
use crate::subagents::SubagentStatus;
use crate::subagents::SubagentStatusEntry;
use crate::subagents::subagent_display_label;
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::registry::PtySessionManager;
use chrono::Utc;
use diapason_config::SubagentSpec;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use tempfile::TempDir;
use tokio::sync::Notify;

mod clock_handoff;
mod observer_ownership;

#[derive(Default)]
struct RecordingObserver {
    children: Mutex<Vec<SubagentStatusEntry>>,
}

impl SubagentProgressObserver for RecordingObserver {
    fn subagent_status_changed(&self, entry: SubagentStatusEntry) {
        self.children.lock().expect("children lock").push(entry);
    }

    fn background_status_changed(&self, _entry: BackgroundSubprocessEntry) {}
}

/// The built-in explorer spec a child record needs.
fn explorer_spec() -> SubagentSpec {
    diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "explorer")
        .expect("explorer spec")
}

fn test_controller_config(workspace_root: PathBuf) -> SubagentControllerConfig {
    let vt_cfg = DiapasonConfig::default();
    let pty_sessions = PtySessionManager::new(workspace_root.clone(), vt_cfg.pty.clone());
    let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
    SubagentControllerConfig {
        workspace_root,
        parent_session_id: "parent-session".to_string(),
        parent_model: "test-model".to_string(),
        parent_provider: "openai".to_string(),
        parent_reasoning_effort: ReasoningEffortLevel::Medium,
        api_key: "test-key".to_string(),
        vt_cfg,
        openai_chatgpt_auth: None,
        depth: 0,
        workspace_gated: false,
        exec_sessions,
        output_reader: None,
        pty_manager: pty_sessions.manager().clone(),
        managed_background_runtime: false,
        lock_scope: None,
    }
}

fn nested_controller_config(workspace_root: PathBuf, parent_session_id: &str) -> SubagentControllerConfig {
    let mut config = test_controller_config(workspace_root);
    config.parent_session_id = parent_session_id.to_string();
    config.depth = 1;
    // Mirror `child_controller_for`: a nested scope runs under the managed
    // background runtime and therefore never owns background work.
    config.managed_background_runtime = true;
    config
}

fn child_record(
    id: &str,
    parent_thread_id: &str,
    spec: &SubagentSpec,
    status: SubagentStatus,
    depth: usize,
) -> ChildRecord {
    ChildRecord {
        id: id.to_string(),
        session_id: format!("{id}-session"),
        parent_thread_id: parent_thread_id.to_string(),
        spec: spec.clone(),
        display_label: subagent_display_label(spec),
        status,
        background: false,
        depth,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        completed_at: status.is_terminal().then_some(Utc::now()),
        summary: None,
        error: None,
        archive_metadata: None,
        archive_path: None,
        transcript_path: None,
        effective_config: None,
        stored_messages: Vec::new(),
        last_prompt: Some(format!("prompt-{id}")),
        queued_prompts: VecDeque::new(),
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        thread_handle: None,
        handle: None,
        notify: Arc::new(Notify::new()),
        worktree_path: None,
        worktree_claim: 0,
        child_controller: None,
        closed: false,
    }
}

/// A route adopted after the parent joined a tree publishes under that
/// tree's key, not under a key resolved before the join.
///
/// The attachment resolves the fallback key before it takes the tree's
/// attachment guard, because that resolution may await the parent's session
/// lock. An attachment that grafts the parent in the meantime moves the
/// registry and the key together, so adopting the fresh registry with the
/// stale key would deliver a child's updates to a session that registered
/// no observer for them.
#[tokio::test]
async fn an_adopted_route_uses_the_key_of_its_own_snapshot() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone()))
        .await
        .expect("root controller");
    let middle = SubagentController::new(nested_controller_config(workspace_root.clone(), "middle-session"))
        .await
        .expect("middle controller");
    let leaf = SubagentController::new(nested_controller_config(workspace_root, "leaf-session"))
        .await
        .expect("leaf controller");
    let spec = explorer_spec();

    let stale_fallback = middle.observer_scope_for("leaf-session").await;
    assert_eq!(stale_fallback, "leaf-session", "an ungrafted controller uses the owning session");

    // The middle controller joins the root before the leaf adopts its
    // route, exactly as a concurrent attachment would.
    middle.inherit_progress_observers(&root, "parent-session").await;

    let observer = Arc::new(RecordingObserver::default());
    let _registration = root.register_progress_observer("parent-session", observer.clone());

    leaf.adopt_progress_observers(&middle, stale_fallback);

    let record = child_record("leaf-task", "leaf-session", &spec, SubagentStatus::Running, 2);
    leaf.state.write().await.children.insert("leaf-task".to_string(), record);
    leaf.publish_child_status("leaf-task").await;

    let observed = observer.children.lock().expect("children lock").clone();
    let ids = observed.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, ["leaf-task"], "the route's own key must win over the pre-join key");

    // Shutdown publishes a terminal snapshot into the same observer, so the
    // assertion above must release the lock first.
    root.signal_shutdown().await;
    middle.signal_shutdown().await;
    leaf.signal_shutdown().await;
}

/// A descendant attached before the graft follows its ancestor onto the
/// tree's observers.
///
/// The leaf is attached to the middle controller while the pair is still
/// independent of the root, so it publishes through the middle controller's
/// registry and key. Grafting only the middle controller would leave the
/// leaf there: the root would list the leaf's tasks, because the tree is
/// walked for management, while its observer never received a single
/// lifecycle update for them.
#[tokio::test]
async fn a_descendant_attached_before_the_graft_publishes_through_the_tree_observer() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone()))
        .await
        .expect("root controller");
    let middle = Arc::new(
        SubagentController::new(nested_controller_config(workspace_root.clone(), "middle-session"))
            .await
            .expect("middle controller"),
    );
    let leaf = Arc::new(
        SubagentController::new(nested_controller_config(workspace_root, "leaf-session"))
            .await
            .expect("leaf controller"),
    );
    let spec = explorer_spec();

    // The leaf joins the middle controller while the pair is independent of
    // the root, as a grandchild joins the controller that spawned it.
    let record = child_record("leaf", "middle-session", &spec, SubagentStatus::Running, 2);
    middle.state.write().await.children.insert("leaf".to_string(), record);
    middle
        .attach_child_controller("leaf", Arc::clone(&leaf))
        .await
        .expect("the leaf joins the middle controller");

    let observer = Arc::new(RecordingObserver::default());
    let _registration = root.register_progress_observer("parent-session", observer.clone());

    // The middle controller, and with it the leaf, joins the root.
    let record = child_record("middle", "parent-session", &spec, SubagentStatus::Running, 1);
    root.state.write().await.children.insert("middle".to_string(), record);
    root.attach_child_controller("middle", Arc::clone(&middle))
        .await
        .expect("the middle controller joins the root");

    let record = child_record("leaf-task", "leaf-session", &spec, SubagentStatus::Running, 3);
    leaf.state.write().await.children.insert("leaf-task".to_string(), record);
    leaf.publish_child_status("leaf-task").await;

    let observed = observer.children.lock().expect("children lock").clone();
    let ids = observed.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, ["leaf-task"], "a pre-graft descendant must reach the tree observer");

    // Shutdown publishes terminal snapshots into the same observer, so the
    // assertion above must release the lock first.
    root.signal_shutdown().await;
    middle.signal_shutdown().await;
    leaf.signal_shutdown().await;
}

/// A closed record accepts no new controller.
///
/// A close walks the subtree it can see at that moment and then settles the
/// record. A link published afterwards would own descendants that walk never
/// reached, so no later close could release them and their controller would
/// outlive the tree. The rejection must leave both sides unchanged, and
/// re-attaching the controller a record already names stays idempotent.
#[tokio::test]
async fn a_closed_record_refuses_a_new_controller_link() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = Arc::new(
        SubagentController::new(test_controller_config(workspace_root.clone()))
            .await
            .expect("root controller"),
    );
    let nested = Arc::new(
        SubagentController::new(nested_controller_config(workspace_root, "child-session"))
            .await
            .expect("nested controller"),
    );
    let spec = explorer_spec();

    let mut closed = child_record("child", "parent-session", &spec, SubagentStatus::Closed, 1);
    closed.closed = true;
    root.state.write().await.children.insert("child".to_string(), closed);

    let rejection = root
        .attach_child_controller("child", Arc::clone(&nested))
        .await
        .expect_err("a closed record must not gain a controller");
    assert!(format!("{rejection}").contains("is closed"), "the rejection must name the closure: {rejection}");
    assert!(
        root.state
            .read()
            .await
            .children
            .get("child")
            .expect("the closed record survives the rejection")
            .child_controller
            .is_none(),
        "a rejected attachment must not publish the controller"
    );
    assert!(
        root.descendant_controllers().await.is_empty(),
        "a rejected attachment must not make the closed record's subtree reachable"
    );

    // The shared helper is the other publication path, and it applies the same
    // rule.
    let mut state = root.state.write().await;
    let helper_rejection =
        SubagentController::attach_child_controller_to_state(&mut state, "child", Arc::clone(&nested))
            .expect_err("the shared helper must refuse a closed record too");
    assert!(
        format!("{helper_rejection}").contains("is closed"),
        "the shared helper must name the closure: {helper_rejection}"
    );
    drop(state);

    // A record that already tracks a controller keeps it, closed or not: the
    // link was published while the record was live and re-publishing it is
    // idempotent.
    let mut attached = child_record("attached", "parent-session", &spec, SubagentStatus::Closed, 1);
    attached.closed = true;
    attached.child_controller = Some(Arc::clone(&nested));
    root.state.write().await.children.insert("attached".to_string(), attached);
    root.attach_child_controller("attached", Arc::clone(&nested))
        .await
        .expect("re-attaching the controller the record names stays idempotent");

    nested.signal_shutdown().await;
    root.signal_shutdown().await;
}

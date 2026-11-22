//! Regressions for graft routing and lifecycle-revision handoff.

use super::*;
use crate::config::DiapasonConfig;
use crate::config::types::ReasoningEffortLevel;
use crate::subagents::BackgroundSubprocessEntry;
use crate::subagents::ChildRecord;
use crate::subagents::SubagentControllerConfig;
use crate::subagents::SubagentProgressObserver;
use crate::subagents::SubagentStatus;
use crate::subagents::SubagentStatusEntry;
use crate::subagents::controller_observers::ProgressObserverRegistry;
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

/// A reservation that races the clock handoff never repeats a revision.
///
/// The handoff copies the joining controller's clock into the tree's clock
/// and then moves the controller onto it. A reservation that read the old
/// clock before the copy and incremented it afterwards would hand out a
/// value the copy had already absorbed, so the tree would publish that
/// revision twice and an adapter that drops non-increasing revisions would
/// discard one of the two snapshots.
#[tokio::test]
async fn concurrent_reserves_across_a_clock_handoff_never_repeat_a_revision() {
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    const RESERVERS: usize = 3;
    const RESERVES_PER_RESERVER: usize = 20_000;
    // Bounds the handoff loop so a reserver that panicked reports its
    // failure through the join instead of parking the test forever.
    const HANDOFF_LIMIT: usize = 200_000;

    let temp = TempDir::new().expect("tempdir");
    let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf()))
        .await
        .expect("controller");
    let registry = Arc::new(ProgressObserverRegistry::default());
    let running = AtomicUsize::new(RESERVERS);

    let reserved = std::thread::scope(|scope| {
        let mut reservers = Vec::with_capacity(RESERVERS);
        for _ in 0..RESERVERS {
            let controller = &controller;
            let running = &running;
            reservers.push(scope.spawn(move || {
                let mut revisions = Vec::with_capacity(RESERVES_PER_RESERVER);
                for _ in 0..RESERVES_PER_RESERVER {
                    revisions.push(controller.reserve_lifecycle_revision());
                }
                running.fetch_sub(1, Ordering::SeqCst);
                revisions
            }));
        }
        // Each handoff retires the clock it copies and installs a fresh
        // one, so every iteration opens the window the fix has to close.
        let mut handoffs = 0_usize;
        while running.load(Ordering::SeqCst) > 0 && handoffs < HANDOFF_LIMIT {
            let clock = Arc::new(AtomicU64::new(0));
            controller.progress_route.adopt(Arc::clone(&registry), None, clock);
            handoffs += 1;
            std::hint::spin_loop();
        }
        reservers
            .into_iter()
            .flat_map(|reserver| reserver.join().expect("a reserver must not panic"))
            .collect::<Vec<u64>>()
    });

    let mut distinct = reserved.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        reserved.len(),
        "a handoff handed out {} of {} revisions twice",
        reserved.len() - distinct.len(),
        reserved.len()
    );
    assert_eq!(
        distinct.last().copied(),
        Some(reserved.len() as u64 - 1),
        "the revisions must stay contiguous across every handoff"
    );
}

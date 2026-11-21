//! Walks a controller tree and joins an attached controller's subtree to it.
//!
//! The controller only tracks its own children, so anything that has to reason
//! about the whole owned tree lives here: the breadth-first traversal adapters
//! use, and the graft that gives every controller in one tree a single child-id
//! allocator.
use anyhow::{Result, anyhow, bail};
use std::sync::Arc;

use super::controller_spawn_run;
use super::{ControllerState, SubagentController, current_subagent_invocation_context};

impl SubagentController {
    /// Returns every child-scoped controller reachable from this controller's
    /// tracked children, recursively.
    ///
    /// Grandchildren spawned by a child run inside that child's own
    /// [`SubagentController`], so a caller that only reads `status_entries`
    /// sees direct children but not the rest of the owned tree. Management and
    /// lifecycle adapters walk this list to reach the whole subtree.
    ///
    /// Controllers are deduplicated by allocation so a re-parented or
    /// re-attached controller cannot produce a cycle, and the returned order
    /// is breadth-first from the nearest children outwards.
    pub async fn descendant_controllers(&self) -> Vec<Arc<SubagentController>> {
        let mut descendants = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut frontier = {
            let state = self.state.read().await;
            state
                .children
                .values()
                .filter_map(|record| record.child_controller.clone())
                .collect::<Vec<_>>()
        };
        while !frontier.is_empty() {
            let mut next = Vec::new();
            for controller in frontier.drain(..) {
                if !visited.insert(Arc::as_ptr(&controller) as usize) {
                    continue;
                }
                let nested = {
                    let state = controller.state.read().await;
                    state
                        .children
                        .values()
                        .filter_map(|record| record.child_controller.clone())
                        .collect::<Vec<_>>()
                };
                next.extend(nested);
                descendants.push(controller);
            }
            frontier = next;
        }
        descendants
    }

    /// Shares child-id allocation with the controller that owns this one.
    ///
    /// Task ids are generated from the agent kind and a millisecond clock, so
    /// two controllers in one tree can propose the same id at the same instant.
    /// Each controller only holds its own children, but the adapter flattens
    /// the tree and routes a task id to the first controller that claims it, so
    /// a duplicate would list one task twice and could send an output read or a
    /// cancel to the wrong task. Every controller in a tree therefore allocates
    /// from the root's scope.
    ///
    /// Ids this controller has already allocated travel with its slot, because
    /// attaching a controller that has children adopts the slot rather than
    /// replacing it. Callers must therefore inherit only a controller that has
    /// not been published: this replaces an unallocated controller's own slot
    /// with the parent's.
    pub(crate) fn inherit_child_id_scope(&self, parent: &SubagentController) {
        let parent_slot = Self::child_id_slot_of(parent);
        *self.child_id_scope.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = parent_slot;
    }

    /// Joins the parent tree's adoption and persistence locks.
    ///
    /// A child-scoped controller starts from a config that names no scope,
    /// exactly as it starts with its own child-id slot and observer registry.
    /// It must join the parent's tree for both, so grafting and ledger saves
    /// exclude siblings the way a root controller excludes itself.
    pub(crate) fn inherit_lock_scope(&mut self, parent: &SubagentController) {
        self.lock_scope = Arc::clone(parent.lock_scope());
    }

    /// Reads the slot a controller shares with its whole subtree.
    ///
    /// The slot object, not the scope inside it, is what a child adopts, so
    /// writing the tree scope into one slot redirects every controller that
    /// already shares it.
    pub(super) fn child_id_slot(&self) -> Arc<controller_spawn_run::ChildIdScopeSlot> {
        Self::child_id_slot_of(self)
    }

    fn child_id_slot_of(controller: &SubagentController) -> Arc<controller_spawn_run::ChildIdScopeSlot> {
        Arc::clone(
            &controller
                .child_id_scope
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// Collects the shared state of a controller and everything it reaches.
    ///
    /// A controller's outer `Arc` is not its identity: [`SubagentController`]
    /// is `Clone`, so two separate outer `Arc`s can wrap clones of one
    /// controller and share its inner state. Cycle detection therefore compares
    /// the inner state `Arc`s, and the visited set is keyed on them so an
    /// existing cycle cannot make the walk diverge.
    async fn reachable_controller_states(controller: &SubagentController) -> std::collections::HashSet<usize> {
        let mut visited = std::collections::HashSet::new();
        let mut frontier = vec![Arc::clone(&controller.state)];
        while let Some(state) = frontier.pop() {
            if !visited.insert(Arc::as_ptr(&state) as usize) {
                continue;
            }
            let nested = {
                let guard = state.read().await;
                guard
                    .children
                    .values()
                    .filter_map(|record| record.child_controller.as_ref())
                    .map(|child| Arc::clone(&child.state))
                    .collect::<Vec<_>>()
            };
            frontier.extend(nested);
        }
        visited
    }

    /// Attaches an already-created child-scoped controller to a tracked child.
    ///
    /// The nested-delegation loop attaches the controller as soon as the child
    /// run creates it so grandchildren stay reachable across resumes. Fails
    /// closed for an unknown child so a controller can never be grafted onto a
    /// record this controller does not track.
    ///
    /// The attached controller's whole subtree joins this tree's child-id
    /// allocation before its record is published, so a caller that built it
    /// independently cannot introduce an id the tree already holds. Ids it had
    /// already allocated stay reserved tree-wide and every descendant it has
    /// already created follows it onto this tree's allocator.
    ///
    /// The joining subtree also publishes through this tree's observers. Its
    /// controllers adopt this tree's registry, lifecycle clock and observer
    /// key before the link is published, including the descendants it had
    /// already created: those keep publishing through the registry of the
    /// controller they were attached to, and the tree that lists their tasks
    /// would otherwise never receive their lifecycle updates.
    ///
    /// An id already held on both sides is rejected rather than absorbed: it
    /// has been published by the joining subtree and the tree, so no suffix
    /// can separate them, and the adapter's first-match routing would resolve
    /// it to one controller only. Rejection leaves the controller, this tree
    /// and the child record unchanged.
    ///
    /// The joining controller must also share this tree's lock scope. The
    /// graft holds that scope's adoption lock to exclude concurrent
    /// allocation, so a controller bound to a different scope cannot be
    /// grafted safely and is rejected with the same fail-closed guarantee.
    /// Every controller created by this tree inherits the scope, so this only
    /// refuses an independently constructed controller with an injected
    /// scope.
    ///
    /// The attachment is also refused before anything changes when it would
    /// close a cycle: attaching this controller, one of its ancestors, or one
    /// of its descendants would make the tree unreachable. The check compares
    /// the shared controller-state `Arc`s, so a clone wrapped in a second outer
    /// `Arc` is still recognised as the same controller.
    ///
    /// A record that already tracks a controller cannot be re-pointed at a
    /// different one. The displaced controller keeps its own children, which
    /// this tree would no longer reach, so a running grandchild would be
    /// stranded. Re-attaching the controller the record already names remains
    /// valid.
    ///
    /// The disjointness checks and the publication run under the tree's
    /// attachment guard, so they are one critical section. Without that, two
    /// attachments in opposite directions could each pass its check before
    /// either published their link, which creates an `Arc` cycle that leaks the
    /// subtree and never terminates a traversal. A controller bound to a
    /// different lock scope can never be published (the child-id adoption
    /// rejects it), so this tree's guard serialises every attachment that can
    /// succeed.
    pub async fn attach_child_controller(&self, child_id: &str, controller: Arc<SubagentController>) -> Result<()> {
        let (attached, owning_session_id) = {
            let state = self.state.read().await;
            let record = state
                .children
                .get(child_id)
                .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
            (
                record.child_controller.as_ref().map(|attached| Arc::clone(&attached.state)),
                record.parent_thread_id.clone(),
            )
        };
        if let Some(attached) = attached.as_ref()
            && !Arc::ptr_eq(attached, &controller.state)
        {
            bail!("Subagent {child_id} is already attached to another controller");
        }
        // The fallback observer key is resolved before the attachment guard,
        // because it may await the parent controller's session lock. The guard
        // must not be held across that wait: another attachment in the tree
        // could depend on this controller making progress. The key that is
        // actually adopted is resolved again from the route this tree publishes
        // through under the guard, because an attachment that grafted this
        // controller in the meantime moved both its registry and its key.
        let fallback_observer_scope = self.observer_scope_for(&owning_session_id).await;
        // Everything from the cycle check to the published link is one critical
        // section, so no opposing attachment can pass the check in between.
        let _attachment_guard = self.lock_scope().attachment().lock().await;
        // The record is re-checked under the guard: it may have been attached
        // between the fast rejection above and this point. An idempotent
        // re-attachment skips the cycle check for the same reason it does
        // below: the record already names the controller, so the two subtrees
        // necessarily overlap.
        let already_attached = {
            let state = self.state.read().await;
            let record = state
                .children
                .get(child_id)
                .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
            match record.child_controller.as_ref() {
                Some(attached) if Arc::ptr_eq(&attached.state, &controller.state) => true,
                Some(_) => bail!("Subagent {child_id} is already attached to another controller"),
                None => false,
            }
        };
        if !already_attached {
            let incoming = Self::reachable_controller_states(&controller).await;
            let tree = Self::reachable_controller_states(self).await;
            if !incoming.is_disjoint(&tree) {
                bail!("Subagent {child_id} cannot be attached: the controller is already part of this tree");
            }
        }
        // A test parks the first of two opposing attachments here, after its
        // cycle check passed and while it still holds the attachment guard, to
        // prove the guard — not an earlier check — is what excludes the second.
        #[cfg(test)]
        attachment_checkpoints::pass(&self.state).await;
        // Every controller the joining subtree already reaches moves onto this
        // tree's publication route with it. A descendant attached before this
        // graft still publishes through the registry and clock it adopted then,
        // so leaving it behind would let the tree list its tasks while never
        // delivering their lifecycle updates to the tree's observers. The walk
        // runs before the state lock so no second controller state lock is held
        // while this tree's is.
        let joining_subtree = controller.descendant_controllers().await;
        // The record is re-checked once more and the incoming allocator adopted
        // while the state write lock is held. The adoption takes only the
        // tree's adoption lock, so this order matches every other path that
        // holds a controller state lock before adopting.
        let mut state = self.state.write().await;
        {
            let record = state
                .children
                .get(child_id)
                .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
            if let Some(attached) = record.child_controller.as_ref()
                && !Arc::ptr_eq(&attached.state, &controller.state)
            {
                bail!("Subagent {child_id} is already attached to another controller");
            }
        }
        if let Err(rejection) = controller.child_id_slot().adopt_into(&self.child_id_slot()) {
            bail!("Subagent {child_id} cannot join the tree allocator: {rejection}");
        }
        // The joining subtree must publish through this tree's registry: an
        // independently constructed controller otherwise keeps its own, and
        // observers registered here would never see its later transitions. One
        // snapshot of this tree's route serves the whole subtree, so every
        // controller in it publishes through the same registry, clock and key.
        let parent_route = self.progress_route_snapshot();
        controller.adopt_progress_route(&parent_route, fallback_observer_scope.as_str());
        for descendant in joining_subtree {
            descendant.adopt_progress_route(&parent_route, fallback_observer_scope.as_str());
        }
        Self::attach_child_controller_to_state(&mut state, child_id, controller)
    }

    /// Publishes `controller` on a tracked child record, refusing to replace one.
    ///
    /// Every attachment path shares this rule: a record that already names a
    /// different controller must keep it. The displaced controller owns the
    /// children it tracks, and this tree would no longer reach them, so a
    /// running grandchild would be stranded. Re-publishing the controller the
    /// record already names is idempotent, matching [`Self::attach_child_controller`].
    ///
    /// The comparison uses the shared controller-state `Arc`, not the outer
    /// `Arc`, because [`SubagentController`] is `Clone`: two outer handles can
    /// wrap clones of one controller.
    pub(super) fn attach_child_controller_to_state(
        state: &mut ControllerState,
        child_id: &str,
        controller: Arc<SubagentController>,
    ) -> Result<()> {
        let record = state
            .children
            .get_mut(child_id)
            .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
        match record.child_controller.as_ref() {
            Some(attached) if Arc::ptr_eq(&attached.state, &controller.state) => return Ok(()),
            Some(_) => bail!("Subagent {child_id} is already attached to another controller"),
            None => {}
        }
        record.child_controller = Some(controller);
        Ok(())
    }

    pub(super) async fn ensure_child_owned_by_invocation(&self, target: &str) -> Result<()> {
        let Some(context) = current_subagent_invocation_context() else {
            return Ok(());
        };
        let state = self.state.read().await;
        let record = state
            .children
            .get(target)
            .ok_or_else(|| anyhow!("Unknown subagent id {target}"))?;
        if record.parent_thread_id != context.parent_session_id {
            bail!("Unknown subagent id {target}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
}

/// Test-only rendezvous points inside one attachment's critical section.
///
/// The attachment guard is the only thing that stops two opposing edges from
/// both passing the cycle check. Parking an attachment after that check and
/// before the link is published makes the guard itself observable, so a test
/// can prove the second attachment waits rather than passing its own check.
#[cfg(test)]
pub(crate) mod attachment_checkpoints {
    use super::*;
    use std::sync::{Mutex, OnceLock, Weak};

    struct Checkpoint {
        state: Weak<tokio::sync::RwLock<ControllerState>>,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    fn checkpoints() -> &'static Mutex<Vec<Checkpoint>> {
        static CHECKPOINTS: OnceLock<Mutex<Vec<Checkpoint>>> = OnceLock::new();
        CHECKPOINTS.get_or_init(|| Mutex::new(Vec::new()))
    }

    /// Arms a checkpoint that holds the next attachment of `state`.
    ///
    /// Returns the signal that the attachment reached the checkpoint and the
    /// signal that releases it.
    pub(crate) fn arm(
        state: &Arc<tokio::sync::RwLock<ControllerState>>,
    ) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut registry = checkpoints().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.retain(|checkpoint| checkpoint.state.strong_count() > 0);
        registry.push(Checkpoint {
            state: Arc::downgrade(state),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        (entered, release)
    }

    /// Parks here when `state` has an armed checkpoint.
    pub(super) async fn pass(state: &Arc<tokio::sync::RwLock<ControllerState>>) {
        let checkpoint = {
            let mut registry = checkpoints().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.retain(|checkpoint| checkpoint.state.strong_count() > 0);
            registry
                .iter()
                .position(|checkpoint| std::ptr::eq(checkpoint.state.as_ptr(), Arc::as_ptr(state)))
                .map(|index| registry.remove(index))
        };
        let Some(checkpoint) = checkpoint else {
            return;
        };
        checkpoint.entered.notify_one();
        checkpoint.release.notified().await;
    }
}

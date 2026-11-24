//! Walks a controller tree and joins an attached controller's subtree to it.
//!
//! The controller only tracks its own children, so anything that has to reason
//! about the whole owned tree lives here: the breadth-first traversal adapters
//! use, and the graft that gives every controller in one tree a single child-id
//! allocator.
use anyhow::{Result, anyhow, bail};
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::DiapasonConfig;
use crate::core::threads::ThreadRuntimeHandle;
use crate::utils::session_archive::SessionArchiveMetadata;

use super::controller_spawn_run;
use super::{ControllerState, SubagentController};

/// The runtime a prepared child run publishes onto its record.
///
/// These fields describe a runner that has started, so they travel together
/// and are published only once the run's controller attachment has been
/// accepted.
pub(crate) struct ChildRuntimePublication {
    pub(crate) archive_metadata: SessionArchiveMetadata,
    pub(crate) archive_path: PathBuf,
    pub(crate) effective_config: DiapasonConfig,
    pub(crate) thread_handle: ThreadRuntimeHandle,
    pub(crate) child_controller: Option<Arc<SubagentController>>,
}

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
    /// A closed record accepts no new controller at all. Its close walked the
    /// subtree it could see at that moment, so a link published afterwards
    /// would own a subtree nothing will ever close.
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
        attachment_checkpoints::pass(&self.state, attachment_checkpoints::AttachmentStage::AfterCycleCheck).await;
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
            // A record a close already settled must not gain a controller. The
            // close ran its subtree cascade against a snapshot taken before
            // this link existed, so a late link would own grandchildren no
            // close will ever reach. A settled record cannot own a new subtree
            // for the same reason, and a re-attachment of the controller the
            // record already names stays idempotent.
            if record.child_controller.is_none() && (record.closed || record.status.is_terminal()) {
                bail!("Subagent {child_id} is closed; refusing to attach a controller");
            }
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
        // A test parks the attachment here, with the state lock held and every
        // await behind it, to prove the guard is still held at the point the
        // link is published.
        #[cfg(test)]
        attachment_checkpoints::pass(&self.state, attachment_checkpoints::AttachmentStage::BeforePublication).await;
        Self::attach_child_controller_to_state(&mut state, child_id, controller)
    }

    /// Publishes a child run's runtime, serialising its controller link with a
    /// concurrent graft of this controller into a larger tree.
    ///
    /// A graft takes this tree's attachment guard, snapshots the subtree it
    /// will move, and only then adopts this controller's route and publishes
    /// the link. A child link published inside that window would not be in the
    /// snapshot, so it would keep the pre-graft route and its lifecycle
    /// updates would never reach the tree's observers. Taking the same guard
    /// first makes the two exclusive: either the link is published before the
    /// snapshot and the graft adopts it with the rest of the subtree, or the
    /// graft has completed and this link adopts the route the graft published.
    ///
    /// The fallback observer key is resolved before the guard, because it may
    /// await this controller's session lock, which the guard must not span.
    pub(super) async fn publish_child_runtime_guarded(
        &self,
        child_id: &str,
        runtime: ChildRuntimePublication,
        owning_session_id: &str,
    ) -> Result<()> {
        let fallback_observer_scope = if runtime.child_controller.is_some() {
            self.observer_scope_for(owning_session_id).await
        } else {
            String::new()
        };
        let _attachment_guard = if runtime.child_controller.is_some() {
            Some(self.lock_scope().attachment().lock().await)
        } else {
            None
        };
        if let Some(controller) = runtime.child_controller.as_ref() {
            // Re-adopt under the guard: a graft may have moved this
            // controller's route since the child controller inherited it at
            // construction.
            let parent_route = self.progress_route_snapshot();
            controller.adopt_progress_route(&parent_route, fallback_observer_scope.as_str());
        }
        let mut state = self.state.write().await;
        Self::publish_child_runtime(&mut state, child_id, runtime)
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
        // The record was settled before this link was published. Its close ran
        // the subtree cascade against a snapshot taken without this
        // controller, so the link would own descendants no close reaches. The
        // rejection leaves the record unchanged.
        if record.closed || record.status.is_terminal() {
            bail!("Subagent {child_id} is closed; refusing to attach a controller");
        }
        record.child_controller = Some(controller);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod attachment_checkpoints;
#[cfg(test)]
mod tests;

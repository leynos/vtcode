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
    pub async fn attach_child_controller(&self, child_id: &str, controller: Arc<SubagentController>) -> Result<()> {
        let attached = {
            let state = self.state.read().await;
            let record = state
                .children
                .get(child_id)
                .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
            record.child_controller.as_ref().map(|attached| Arc::clone(&attached.state))
        };
        let idempotent = attached
            .as_ref()
            .is_some_and(|attached| Arc::ptr_eq(attached, &controller.state));
        match attached {
            Some(_) if !idempotent => {
                bail!("Subagent {child_id} is already attached to another controller");
            }
            Some(_) => {}
            None => {
                let incoming = Self::reachable_controller_states(&controller).await;
                let tree = Self::reachable_controller_states(self).await;
                if !incoming.is_disjoint(&tree) {
                    bail!("Subagent {child_id} cannot be attached: the controller is already part of this tree");
                }
            }
        }
        // The record is re-checked and the incoming allocator adopted while the
        // state write lock is held, so two attachments cannot both observe an
        // empty record and then publish over one another. The adoption takes
        // only the tree's adoption lock, so this order matches every other
        // path that holds a controller state lock before adopting.
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
        Self::attach_child_controller_to_state(&mut state, child_id, controller)
    }

    pub(super) fn attach_child_controller_to_state(
        state: &mut ControllerState,
        child_id: &str,
        controller: Arc<SubagentController>,
    ) -> Result<()> {
        let record = state
            .children
            .get_mut(child_id)
            .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
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

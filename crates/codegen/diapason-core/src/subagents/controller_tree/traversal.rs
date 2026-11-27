//! Walking a controller tree.
//!
//! The controller only tracks its own children, so the walks that have to reach
//! the whole owned subtree live here: the breadth-first list adapters use, and
//! the reachable-state set the disjointness checks compare.

use std::sync::Arc;

use super::SubagentController;

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

    /// Collects the shared state of a controller and everything it reaches.
    ///
    /// A controller's outer `Arc` is not its identity: [`SubagentController`]
    /// is `Clone`, so two separate outer `Arc`s can wrap clones of one
    /// controller and share its inner state. Cycle detection therefore compares
    /// the inner state `Arc`s, and the visited set is keyed on them so an
    /// existing cycle cannot make the walk diverge.
    pub(super) async fn reachable_controller_states(
        controller: &SubagentController,
    ) -> std::collections::HashSet<usize> {
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
}

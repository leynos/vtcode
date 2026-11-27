//! Which tree already owns a controller, so a second graft can be refused.
//!
//! A record stores its controller; nothing stored by the controller names the
//! record that lists it. A tree therefore cannot discover a controller's parent
//! by walking, and the downward disjointness check in `attach_child_controller`
//! cannot see it either. The owner is recorded when the link is published and
//! read back when another tree tries to publish the same controller.
use std::sync::{Arc, Weak};

use tokio::sync::RwLock;

use super::SubagentController;
use crate::subagents::ControllerState;

impl SubagentController {
    /// Reports whether another live tree already lists `controller`.
    ///
    /// The link lives on the record, in the tree that owns it, so nothing this
    /// tree can reach names the controller's parent. The controller therefore
    /// records the tree it was published into when its link is published, and
    /// that record is what a second graft is checked against.
    ///
    /// The recorded tree is the owner's shared state `Arc`, so it is compared
    /// the way every other controller identity is: two outer handles wrapping
    /// clones of one controller still name the same tree. A tree whose state
    /// has been dropped no longer holds any link, so its ownership expires with
    /// it.
    pub(super) fn is_owned_by_another_tree(&self, tree: &Arc<RwLock<ControllerState>>) -> bool {
        let owner = self.attachment_owner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        owner.upgrade().is_some_and(|owner| !Arc::ptr_eq(&owner, tree))
    }

    /// Records `tree` as the tree this controller's link is published into.
    ///
    /// Called under the publishing tree's attachment guard, once the link is
    /// published, so the ownership a second graft observes is the one the
    /// published link established.
    pub(super) fn record_attachment_owner(&self, tree: &Arc<RwLock<ControllerState>>) {
        *self.attachment_owner.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::downgrade(tree);
    }
}

//! Publishing the controller link a child run carries onto its record.
//!
//! The link is written under the tree's attachment guard, so it is serialized
//! with a graft that is moving this tree's controllers onto a larger tree's
//! route. Declared by `mod child_link;` from `controller_tree.rs`, which
//! re-exports [`ChildRuntimePublication`] for its callers.

use anyhow::{Result, anyhow, bail};
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::DiapasonConfig;
use crate::core::threads::ThreadRuntimeHandle;
use crate::utils::session_archive::SessionArchiveMetadata;

use super::super::{ControllerState, SubagentController};

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
    /// Publishes a child run's runtime, serializing its controller link with a
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
    pub(in crate::subagents) async fn publish_child_runtime_guarded(
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
            // The same ownership rule as the standalone attachment. A record
            // that already lists this controller was published here, so its
            // owner is this tree and the re-publication stays idempotent; a
            // controller owned elsewhere is refused before its route is
            // adopted, leaving both the route and the record unchanged.
            if controller.is_owned_by_another_tree(&self.state) {
                bail!("Subagent {child_id} cannot be attached: the controller already belongs to another tree");
            }
            // Re-adopt under the guard: a graft may have moved this
            // controller's route since the child controller inherited it at
            // construction.
            let parent_route = self.progress_route_snapshot();
            controller.adopt_progress_route(&parent_route, fallback_observer_scope.as_str());
        }
        // Kept so the tree that owns the link can be recorded once it exists:
        // the publication below consumes the runtime's own handle.
        let linked = runtime.child_controller.clone();
        {
            let mut state = self.state.write().await;
            Self::publish_child_runtime(&mut state, child_id, runtime)?;
        }
        // The ownership is recorded only after the record carries the link, so
        // a rejected publication leaves the controller unowned and the record
        // untouched.
        if let Some(controller) = linked.as_ref() {
            controller.record_attachment_owner(&self.state);
        }
        Ok(())
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
    ///
    /// This helper cannot name the tree it publishes into, so the cross-tree
    /// rule is applied by the two callers that can
    /// ([`Self::attach_child_controller`] and
    /// [`Self::publish_child_runtime_guarded`]) before they reach it.
    pub(in crate::subagents) fn attach_child_controller_to_state(
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

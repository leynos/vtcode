//! Walks a controller tree and joins an attached controller's subtree to it.
//!
//! The controller only tracks its own children, so anything that has to reason
//! about the whole owned tree lives here: the breadth-first traversal adapters
//! use, and the graft that gives every controller in one tree a single child-id
//! allocator.
#![allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use futures::future::select_all;
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, Weak};
use tokio::sync::{Notify, RwLock};

use crate::config::DiapasonConfig;
use crate::config::types::ReasoningEffortLevel;
use crate::core::agent::runner::{AgentRunner, RunnerSettings};
use crate::core::agent::task::Task;
use crate::core::threads::{ThreadBootstrap, ThreadId, ThreadRuntimeHandle, ThreadSnapshot};
use crate::hooks::{LifecycleHookEngine, SessionStartTrigger};
use crate::llm::provider::Message;
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::pty::{PtyManager, PtySize};
use crate::utils::session_archive::{SessionArchive, find_session_by_identifier};
use diapason_config::SubagentSpec;
use diapason_config::auth::OpenAIChatGptAuthHandle;

use self::background::*;
use self::config::*;
use self::constants::*;
use self::discovery::discover_controller_subagents;
use self::model::*;
use diapason_config::subagents::SUBAGENT_HARD_CONCURRENCY_LIMIT;

#[allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
use super::*;

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
    pub async fn attach_child_controller(&self, child_id: &str, controller: Arc<SubagentController>) -> Result<()> {
        let mut state = self.state.write().await;
        if !state.children.contains_key(child_id) {
            bail!("Unknown subagent id {child_id}");
        }
        if let Err(duplicate) = controller.child_id_slot().adopt_into(&self.child_id_slot()) {
            bail!(
                "Subagent {child_id} cannot join the tree allocator: task id {duplicate} is already allocated on both sides"
            );
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

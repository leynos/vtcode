//! Session ownership policy for subagent tasks.
//!
//! The controller is shared by several transports: the local TUI, the ACP
//! adapter, and provider tools. Each command that reads, stops, or resumes a
//! task therefore has to answer one question before it acts: does the caller
//! own this task? That decision is domain policy, not transport policy, so it
//! lives here and every task-addressing command takes the caller's authority as
//! an explicit parameter. An ambient task-local context is not consulted: a
//! command that happens to run outside an invocation would otherwise silently
//! treat an unowned task as its own. The projections that select a caller's
//! tasks, such as the background status queries, take the same authority so
//! their visibility rule cannot drift from the commands' rule.
//!
//! Two ownership shapes exist because the two task kinds are created
//! differently. A managed background process records the session that
//! requested it in `owner_session_id`, so ownership is equality. A child agent
//! records its parent thread, and a grandchild's record names its parent's
//! session rather than the session that started the whole delegation, so
//! ownership follows the delegation tree down from the caller's session.

use std::collections::HashSet;

use anyhow::{Result, bail};

use super::background::background_record_id;
use super::{BackgroundSubprocessEntry, ChildRecord, SubagentController, SubagentStatusEntry};

/// The authority a caller presents when it asks the controller for a task.
///
/// `Session` is a transport session acting for itself: it may see and act on
/// only the tasks it owns. `SessionTree` is the same authority after the
/// transport has resolved the tasks the session owns across the whole
/// delegation tree. `Workspace` is the local workspace operator, which is the
/// user at this machine's keyboard or CLI; it owns the workspace's tasks and
/// is the authority the local callers present.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskOwner<'a> {
    /// A session acting for itself.
    Session(&'a str),
    /// A session acting on the tasks it owns across its delegation tree.
    ///
    /// A grandchild record lives in the child-scoped controller that spawned
    /// it, and that record names the child's session rather than the session
    /// that started the delegation. The child-scoped controller holds no link
    /// back to its ancestors, so it cannot prove the ancestry from its own
    /// snapshot. The transport does hold the session's root controller, so it
    /// resolves the owned set by walking only that session's tree and presents
    /// it here. The set is therefore already narrowed to the caller's tasks;
    /// it is never a wider authority than `Session`.
    SessionTree(&'a HashSet<&'a str>),
    /// The local workspace operator.
    Workspace,
}

impl TaskOwner<'_> {
    /// Reports whether a background record with this owner belongs to the caller.
    ///
    /// A background record with no owner was started by the workspace operator
    /// before any session claimed it. It belongs to no session, so every
    /// session sees it as foreign rather than as a shared task.
    ///
    /// A resolved tree set names child tasks, and a background record is owned
    /// by the session that launched it, so presenting one for a background
    /// record can never prove ownership and is refused.
    #[must_use]
    pub fn background_owned(self, owner_session_id: Option<&str>) -> bool {
        match self {
            Self::Session(session_id) => owner_session_id == Some(session_id),
            Self::SessionTree(_) => false,
            Self::Workspace => true,
        }
    }
}

/// Selects the managed background records a session owns.
///
/// Managed background processes carry an explicit `owner_session_id`, so
/// unlike child agents their ownership neither follows `parent_thread_id` nor
/// transitions through a child's session. A record with no owner belongs to no
/// session and stays invisible to every session-scoped caller.
#[must_use]
pub fn owned_background_ids<'a>(session_id: &str, entries: &'a [BackgroundSubprocessEntry]) -> HashSet<&'a str> {
    entries
        .iter()
        .filter(|entry| entry.owner_session_id.as_deref() == Some(session_id))
        .map(|entry| entry.id.as_str())
        .collect()
}

/// Resolves a caller-supplied background id to the canonical id this session
/// owns.
///
/// An exact match wins. Otherwise the first record owned by `session_id` whose
/// pre-owner-scoping id (`background_record_id(entry.agent_name)`) equals
/// `target` is returned. That fallback lets a client still address a record it
/// retained from before owner-scoped ids were introduced. It only considers
/// records `session_id` owns, so it never widens ownership to another session's
/// record.
#[must_use]
pub fn resolve_owned_background_id<'a>(
    session_id: &str,
    entries: &'a [BackgroundSubprocessEntry],
    target: &str,
) -> Option<&'a str> {
    if let Some(entry) = entries
        .iter()
        .find(|entry| entry.id == target && entry.owner_session_id.as_deref() == Some(session_id))
    {
        return Some(entry.id.as_str());
    }
    entries
        .iter()
        .find(|entry| {
            entry.owner_session_id.as_deref() == Some(session_id)
                && background_record_id(entry.agent_name.as_str()) == target
        })
        .map(|entry| entry.id.as_str())
}

/// The borrowed identity fields ownership follows for one child task.
///
/// The order is `(child id, child session id, parent thread id)`. Ownership
/// only needs these three borrowed strings, so the walk never clones a status
/// entry's labels, timings or other presentation fields.
type ChildIdentity<'a> = (&'a str, &'a str, &'a str);

/// Walks borrowed child identities and returns the ids the session owns.
///
/// A delegation tree is layered: the session that starts a delegation owns the
/// child it named, that child's session owns the grandchildren it started, and
/// so on. A caller that owns the root therefore owns everything below it,
/// which is why the walk repeats until it stops discovering new sessions.
fn owned_child_ids_in<'a>(session_id: &str, children: &[ChildIdentity<'a>]) -> HashSet<&'a str> {
    let mut owned_sessions = HashSet::from([session_id]);
    let mut owned_ids = HashSet::new();
    loop {
        let before = owned_ids.len();
        for &(id, child_session_id, parent_thread_id) in children {
            if owned_sessions.contains(parent_thread_id) {
                let _ = owned_ids.insert(id);
                let _ = owned_sessions.insert(child_session_id);
            }
        }
        if owned_ids.len() == before {
            break;
        }
    }
    owned_ids
}

/// Selects the child tasks a session owns, directly or through a descendant.
#[must_use]
pub fn owned_child_ids<'a>(session_id: &str, entries: &'a [SubagentStatusEntry]) -> HashSet<&'a str> {
    let children = entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry.session_id.as_str(), entry.parent_thread_id.as_str()))
        .collect::<Vec<_>>();
    owned_child_ids_in(session_id, &children)
}

impl SubagentController {
    /// Reports whether `session_id` may address `child_id` on this controller.
    ///
    /// This is the question every session-scoped command asks before it acts.
    /// The walk reads this controller's own `children` map, so a grandchild —
    /// which lives in the child-scoped controller that spawned it — is not
    /// owned here. A transport that presents [`TaskOwner::SessionTree`] resolves
    /// the whole tree first; an admission check that must agree with execution
    /// asks this question instead of walking descendants, because a target
    /// admitted from a descendant would then be refused by the command it was
    /// admitted for.
    pub async fn owns_child(&self, session_id: &str, child_id: &str) -> bool {
        let state = self.state.read().await;
        // Borrow only the three identity fields the walk reads: building a
        // status entry here would clone every presentation field per record.
        let children = state
            .children
            .values()
            .map(|record| (record.id.as_str(), record.session_id.as_str(), record.parent_thread_id.as_str()))
            .collect::<Vec<_>>();
        owned_child_ids_in(session_id, &children).contains(child_id)
    }

    /// Returns the session this controller serves.
    ///
    /// A command executed through this controller presents this id as its
    /// [`TaskOwner::Session`] authority: the ACP adapter resolves the
    /// session's own controller and the executor names the invocation's parent
    /// session, which is the value recorded here when the controller was
    /// created for that session. Reading the live value keeps a controller
    /// retargeted at runtime consistent with the records it spawns afterwards.
    pub async fn served_session_id(&self) -> String {
        self.parent_session_id.read().await.clone()
    }

    /// Test-support seam: records a fixture child without launching a run.
    ///
    /// Production only creates a child record from a real spawn, so a dependent
    /// crate's admission regression for nested delegation needs a way to build a
    /// tree in which the grandchild exists only in a child-scoped controller.
    /// The inserted record is what a queued spawn would have produced: the same
    /// id, owning session and parent thread.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub async fn insert_test_child_record(
        &self,
        id: &str,
        session_id: &str,
        parent_thread_id: &str,
        depth: usize,
        spec_name: &str,
    ) -> Result<()> {
        let Some(spec) = diapason_config::builtin_subagents()
            .into_iter()
            .find(|spec| spec.name == spec_name)
        else {
            bail!("Unknown builtin subagent spec {spec_name}");
        };
        let now = chrono::Utc::now();
        let record = ChildRecord {
            id: id.to_string(),
            session_id: session_id.to_string(),
            parent_thread_id: parent_thread_id.to_string(),
            display_label: super::subagent_display_label(&spec),
            spec,
            status: super::SubagentStatus::Running,
            background: false,
            depth,
            created_at: now,
            updated_at: now,
            completed_at: None,
            summary: None,
            error: None,
            archive_metadata: None,
            archive_path: None,
            transcript_path: None,
            effective_config: None,
            stored_messages: Vec::new(),
            last_prompt: Some(format!("fixture task for {id}")),
            queued_prompts: std::collections::VecDeque::new(),
            max_turns: None,
            model_override: None,
            reasoning_override: None,
            thread_handle: None,
            handle: None,
            notify: std::sync::Arc::new(tokio::sync::Notify::new()),
            worktree_path: None,
            child_controller: None,
            closed: false,
        };
        let _previous = self.state.write().await.children.insert(id.to_string(), record);
        Ok(())
    }

    /// Rejects a child task the caller does not own.
    ///
    /// The workspace operator has no session to compare against, so it is
    /// allowed to address any task the controller holds; a missing task is
    /// left to the command's own handling. A session-scoped caller is checked
    /// against the delegation tree, and a foreign or absent target produces the
    /// same not-found error so a transport cannot use the error text to probe
    /// for tasks it cannot see.
    pub(super) async fn ensure_child_owned(&self, owner: TaskOwner<'_>, target: &str) -> Result<()> {
        let owned = match owner {
            // The workspace operator has no session to compare against.
            TaskOwner::Workspace => true,
            TaskOwner::SessionTree(tasks) => tasks.contains(target),
            // The same question [`Self::owns_child`] answers, so an admission
            // check built on that helper cannot admit a target this command
            // refuses.
            TaskOwner::Session(session_id) => self.owns_child(session_id, target).await,
        };
        if owned {
            return Ok(());
        }
        bail!("Unknown subagent id {target}")
    }
}

#[cfg(test)]
mod tests {
    //! Tree ownership: a session owns its children and their descendants.
    use super::*;

    fn background(id: &str, owner_session_id: Option<&str>, agent_name: &str) -> BackgroundSubprocessEntry {
        let now = chrono::Utc::now();
        BackgroundSubprocessEntry {
            id: id.to_string(),
            owner_session_id: owner_session_id.map(str::to_owned),
            session_id: format!("session-for-{id}"),
            exec_session_id: format!("exec-for-{id}"),
            agent_name: agent_name.to_string(),
            display_label: agent_name.to_string(),
            description: agent_name.to_string(),
            source: "builtin".to_string(),
            colour: None,
            status: super::super::BackgroundSubprocessStatus::Running,
            desired_enabled: true,
            created_at: now,
            updated_at: now,
            started_at: None,
            ended_at: None,
            pid: None,
            summary: None,
            error: None,
            archive_path: None,
            transcript_path: None,
        }
    }

    fn child(id: &str, parent_thread_id: &str, session_id: &str) -> SubagentStatusEntry {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "session_id": session_id,
            "parent_thread_id": parent_thread_id,
            "agent_name": "worker",
            "display_label": "Worker",
            "description": "Work",
            "source": "builtin",
            "status": "running",
            "background": false,
            "depth": 1,
            "created_at": "2026-08-14T12:00:00Z",
            "updated_at": "2026-08-14T12:00:00Z",
        }))
        .expect("a child fixture")
    }

    /// Ownership follows the delegation tree, not a flat parent comparison.
    ///
    /// The grandchild is listed before the child that owns it, so one forward
    /// pass over the entries cannot discover it: only the repeated walk does.
    #[test]
    fn a_session_owns_its_children_and_their_descendants() {
        let entries = vec![
            child("grandchild", "child-session", "grandchild-session"),
            child("child", "root-session", "child-session"),
            child("sibling", "other-session", "sibling-session"),
        ];

        let owned = owned_child_ids("root-session", &entries);

        assert!(owned.contains("child"));
        assert!(owned.contains("grandchild"), "a descendant of an owned child is owned too");
        assert!(!owned.contains("sibling"), "another session's delegation is foreign");
    }

    /// A session owns nothing it did not start.
    #[test]
    fn an_unrelated_session_owns_no_task() {
        let entries = vec![child("child", "root-session", "child-session")];

        assert!(owned_child_ids("stranger", &entries).is_empty());
    }

    /// A client that retained the pre-owner-scoping id can still address its
    /// record: the owning session resolves the legacy id to the canonical one.
    #[test]
    fn resolve_owned_background_id_maps_a_legacy_id_to_its_owning_record() {
        let entries = vec![
            background("background-worker@session-b", Some("session-b"), "worker"),
            background("background-worker@session-a", Some("session-a"), "worker"),
        ];

        assert_eq!(
            resolve_owned_background_id("session-a", &entries, "background-worker"),
            Some("background-worker@session-a"),
            "a legacy id must resolve to the caller's canonical record"
        );
        assert_eq!(
            resolve_owned_background_id("session-a", &entries, "background-worker@session-a"),
            Some("background-worker@session-a"),
            "an exact canonical id must win"
        );
    }

    /// The legacy-id fallback must never widen ownership to another session's
    /// record, even when that record is the only match for the legacy id.
    #[test]
    fn resolve_owned_background_id_does_not_widen_ownership_to_another_session() {
        let entries = vec![background("background-worker@session-a", Some("session-a"), "worker")];

        assert_eq!(
            resolve_owned_background_id("session-b", &entries, "background-worker"),
            None,
            "another session's record must not resolve for a caller that owns nothing"
        );
        assert_eq!(
            resolve_owned_background_id("session-b", &entries, "background-worker@session-a"),
            None,
            "an exact id must not resolve when another session owns it"
        );
    }
}

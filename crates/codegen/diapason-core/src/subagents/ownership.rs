//! Session ownership policy for subagent tasks.
//!
//! The controller is shared by several transports: the local TUI, the ACP
//! adapter, and provider tools. Each command that reads, stops, or resumes a
//! task therefore has to answer one question before it acts: does the caller
//! own this task? That decision is domain policy, not transport policy, so it
//! lives here and every command takes the caller's authority as an explicit
//! parameter. An ambient task-local context is not consulted: a command that
//! happens to run outside an invocation would otherwise silently treat an
//! unowned task as its own.
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
use super::{BackgroundSubprocessEntry, ChildRecord, StatusEntryBuilder, SubagentController, SubagentStatusEntry};

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

/// Selects the child tasks a session owns, directly or through a descendant.
///
/// A delegation tree is layered: the session that starts a delegation owns the
/// child it named, that child's session owns the grandchildren it started, and
/// so on. A caller that owns the root therefore owns everything below it,
/// which is why the walk repeats until it stops discovering new sessions.
#[must_use]
pub fn owned_child_ids<'a>(session_id: &str, entries: &'a [SubagentStatusEntry]) -> HashSet<&'a str> {
    let mut owned_sessions = HashSet::from([session_id]);
    let mut owned_ids = HashSet::new();
    loop {
        let before = owned_ids.len();
        for entry in entries {
            if owned_sessions.contains(entry.parent_thread_id.as_str()) {
                let _ = owned_ids.insert(entry.id.as_str());
                let _ = owned_sessions.insert(entry.session_id.as_str());
            }
        }
        if owned_ids.len() == before {
            break;
        }
    }
    owned_ids
}

impl SubagentController {
    /// Rejects a child task the caller does not own.
    ///
    /// The workspace operator has no session to compare against, so it is
    /// allowed to address any task the controller holds; a missing task is
    /// then reported by the command's own lookup. A session-scoped caller is
    /// checked against the delegation tree, and a foreign or absent target
    /// produces the same not-found error so a transport cannot use the error
    /// text to probe for tasks it cannot see.
    pub(super) async fn ensure_child_owned(&self, owner: TaskOwner<'_>, target: &str) -> Result<()> {
        let owned = match owner {
            // The workspace operator has no session to compare against.
            TaskOwner::Workspace => true,
            TaskOwner::SessionTree(tasks) => tasks.contains(target),
            TaskOwner::Session(session_id) => {
                let state = self.state.read().await;
                let entries = state.children.values().map(ChildRecord::build_status_entry).collect::<Vec<_>>();
                owned_child_ids(session_id, &entries).contains(target)
            }
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
    #[test]
    fn a_session_owns_its_children_and_their_descendants() {
        let entries = vec![
            child("child", "root-session", "child-session"),
            child("grandchild", "child-session", "grandchild-session"),
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

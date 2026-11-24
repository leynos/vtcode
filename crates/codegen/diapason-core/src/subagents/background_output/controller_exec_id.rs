//! The exec-session id convention a controller background launch writes.
//!
//! A controller launch names its exec session `exec-<session>-<pid>-<sequence>`,
//! and the background-output reader authorises a restored record's archive by
//! rebuilding that id from the record's own session id. The producer half and
//! the reader half are therefore defined together, so the convention has one
//! definition and the two cannot drift.
//!
//! The helpers are re-exported through `crate::tools::exec_session`, which is
//! the path the exec-session manager and the launch producer already use.

/// The prefix a controller-generated background exec-session id carries.
const CONTROLLER_EXEC_SESSION_PREFIX: &str = "exec-";

/// Builds the exec-session id a controller background launch creates for
/// `session_id`.
///
/// The launch convention is `exec-<session>-<pid>-<sequence>`, with the process
/// id and the launch sequence written as minimal lowercase hexadecimal. The
/// session id may itself contain hyphens and may end in hexadecimal-looking
/// components of its own, so the two trailing components are always the launch
/// pair and [`controller_exec_session_task_id`] reads them off the right.
///
/// This is the producer half of the convention and
/// [`controller_exec_session_task_id`] is the reader half.
#[must_use]
pub(crate) fn controller_exec_session_id(session_id: &str, process_id: u32, sequence: u64) -> String {
    format!("{CONTROLLER_EXEC_SESSION_PREFIX}{session_id}-{process_id:x}-{sequence:x}")
}

/// Returns the task session id a controller launch embedded in
/// `exec_session_id`.
///
/// The rule is the launch convention `exec-<session>-<pid>-<sequence>`: strip
/// the `exec-` prefix and the two trailing components, and accept the result
/// only when rebuilding the id from it with [`controller_exec_session_id`]
/// reproduces the input exactly. The round trip is what makes the rule strict:
/// a non-hexadecimal, uppercase, zero-padded, over-width or extra component is
/// refused rather than tolerated, and an id with no launch pair — every direct
/// exec session, such as `run-1` — yields `None`.
///
/// The process component is deliberately not compared with this process's id. A
/// row restored from the ledger keeps the pair a launch wrote, and a resumed
/// session must still read its own settled task's archive after a restart, so
/// the process that built the id is evidence of the launch rather than a
/// requirement on the current process.
#[must_use]
pub(crate) fn controller_exec_session_task_id(exec_session_id: &str) -> Option<&str> {
    let suffix = exec_session_id.strip_prefix(CONTROLLER_EXEC_SESSION_PREFIX)?;
    let (without_sequence, sequence) = suffix.rsplit_once('-')?;
    let (session_id, process_id) = without_sequence.rsplit_once('-')?;
    let process_id = u32::from_str_radix(process_id, 16).ok()?;
    let sequence = u64::from_str_radix(sequence, 16).ok()?;
    if session_id.is_empty() {
        return None;
    }
    (controller_exec_session_id(session_id, process_id, sequence) == exec_session_id).then_some(session_id)
}

#[cfg(test)]
mod tests {
    use super::{controller_exec_session_id, controller_exec_session_task_id};

    /// The launch convention pairs a task session with the exec id built for it.
    #[test]
    fn controller_exec_session_id_pairs_a_task_session_with_its_exec_session() {
        // A session id shaped like the producer's own: a sanitized base followed
        // by that launch's pid and sequence, both hexadecimal.
        let session_id = "session-a-background-worker-20261122T000000000Z-1f2e-0";
        let exec_session_id = controller_exec_session_id(session_id, 0xabc, 7);
        assert_eq!(exec_session_id, "exec-session-a-background-worker-20261122T000000000Z-1f2e-0-abc-7");
        assert_eq!(
            controller_exec_session_task_id(&exec_session_id),
            Some(session_id),
            "the id must embed the task session it was built for, even when that session ends in hex components"
        );

        // The process component belongs to the launch, not to the reader: a row
        // restored after a restart keeps an earlier process's id and stays
        // readable here.
        let restored = controller_exec_session_id(session_id, std::process::id(), 0);
        assert_eq!(controller_exec_session_task_id(&restored), Some(session_id));

        // Another task's launch names that other task, never this one.
        let foreign = controller_exec_session_id("other-session", 0xabc, 8);
        assert_ne!(controller_exec_session_task_id(&foreign), Some(session_id));

        // A direct exec session names no task.
        assert_eq!(controller_exec_session_task_id("run-1"), None);
        assert_eq!(controller_exec_session_task_id("exec-run-1"), None);

        // Only the canonical spelling the constructor writes is accepted, so an
        // uppercase, zero-padded, non-hexadecimal, empty or extra component is
        // refused.
        assert_eq!(controller_exec_session_task_id("exec-session-base-ABC-1"), None);
        assert_eq!(controller_exec_session_task_id("exec-session-base-0abc-1"), None);
        assert_eq!(controller_exec_session_task_id("exec-session-base-abc-nothex"), None);
        assert_eq!(controller_exec_session_task_id("exec-session-base-abc"), None);
        assert_eq!(controller_exec_session_task_id("exec--abc-1"), None);
        assert_eq!(controller_exec_session_task_id("exec-"), None);
    }
}

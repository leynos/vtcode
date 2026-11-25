//! Pins the launch identity a cancellation is compared and adopted under.
//!
//! Declared by the ledger's test module. A stop governs only the launch it
//! names, so the identity rule decides whether a persisted copy may outrank a
//! cached one or be adopted onto it. A named exec session is that identity. A
//! legacy row names none, so the child session it carries is the identity left;
//! a blank child session never establishes one between two rows.

use super::*;
use serde_json::json;

/// Two blank exec sessions over different child sessions are not one launch.
///
/// A legacy row predates the exec-session field, so a shared blank value
/// cannot establish that a persisted copy and a cached record describe the
/// same process. Before the rule was tightened, an empty id compared equal
/// to another empty id and a stop of one legacy row adopted a cancellation
/// onto an unrelated launch, or let one row's cancelled copy outrank the
/// other's active copy and suppress a task that was still wanted. The child
/// session each row names is the identity that tells them apart.
#[test]
fn an_empty_exec_session_does_not_establish_a_shared_launch() {
    let stopped = persisted_copy_naming(
        "background-worker",
        "",
        "first-child-session",
        false,
        BackgroundSubprocessStatus::Stopped,
    );
    let desired = persisted_copy_naming(
        "background-worker",
        "",
        "second-child-session",
        true,
        BackgroundSubprocessStatus::Running,
    );

    assert!(
        !stopped.cancellation_outranks(&desired),
        "a blank exec session must not let one legacy row stop another"
    );
    assert!(
        !desired.cancellation_outranks(&stopped),
        "the refusal is symmetric: intent alone decides nothing here"
    );
    assert!(
        !stopped.shares_launch_identity_with(&desired),
        "two different child sessions never establish one launch identity"
    );
}

/// Two blank exec sessions over the *same* child session are one launch.
///
/// The identity a legacy row still carries is its child session, and it is
/// minted per launch. A persisted stop and a refreshed copy of that same child
/// session are therefore copies of one launch, and the stop must outrank the
/// refresh; refusing this case would strand a genuine pre-session stop.
#[test]
fn a_shared_legacy_child_session_establishes_one_launch() {
    let stopped = persisted_copy("background-worker", "", false, BackgroundSubprocessStatus::Stopped);
    let desired = persisted_copy("background-worker", "", true, BackgroundSubprocessStatus::Running);

    assert!(stopped.shares_launch_identity_with(&desired), "the shared child session is the legacy identity");
    assert!(stopped.cancellation_outranks(&desired), "the launch's own stop outranks its refresh");
    assert!(!desired.cancellation_outranks(&stopped), "a desired copy never outranks its stop");
}

/// A named exec session is still the launch identity.
///
/// This is the positive control for the refusal above: the same fixtures
/// with a shared session id must make a stop outrank the copy it stopped.
#[test]
fn a_named_exec_session_still_establishes_a_shared_launch() {
    let stopped = persisted_copy("background-worker", "exec-shared", false, BackgroundSubprocessStatus::Stopped);
    let desired = persisted_copy("background-worker", "exec-shared", true, BackgroundSubprocessStatus::Running);

    assert!(stopped.cancellation_outranks(&desired), "a stop of one launch outranks its own refresh");
    assert!(
        !desired.cancellation_outranks(&stopped),
        "a desired copy of the same launch does not outrank its stop"
    );
    assert!(stopped.shares_launch_identity_with(&desired), "the shared session is the identity");
}

/// Different named sessions are never one launch.
///
/// A restart mints a new exec session, so the new launch must not be
/// treated as a copy of the one it replaced.
#[test]
fn different_named_exec_sessions_are_different_launches() {
    let replaced = persisted_copy("background-worker", "exec-old", false, BackgroundSubprocessStatus::Stopped);
    let replacement = persisted_copy("background-worker", "exec-new", true, BackgroundSubprocessStatus::Running);

    assert!(!replaced.shares_launch_identity_with(&replacement), "a restart is a new launch");
    assert!(
        !replaced.cancellation_outranks(&replacement),
        "the stop of a replaced launch must not govern its replacement"
    );
}

/// A stop is never adopted onto a launch that names another session.
///
/// Adoption clears the cached record's desire and arms a close, so
/// applying it across launch identities would stop a live process the
/// persisted row never described. A legacy row naming no session is
/// likewise not a stop of a record that names one.
#[test]
fn a_stop_is_not_adopted_across_launch_identities() {
    let mut record = live_record("background-worker", "exec-live");
    let legacy_stop = persisted_copy("background-worker", "", false, BackgroundSubprocessStatus::Stopped);
    assert!(!legacy_stop.adopt_cancellation_into(&mut record), "a legacy stop is not this launch's stop");
    assert!(record.desired_enabled, "a refused adoption leaves the launch wanting to run");
    assert_eq!(record.status, BackgroundSubprocessStatus::Running, "a refused adoption settles nothing");

    let other_launch_stop =
        persisted_copy("background-worker", "exec-other", false, BackgroundSubprocessStatus::Stopped);
    assert!(!other_launch_stop.adopt_cancellation_into(&mut record), "a different session is a different launch");
    assert!(record.desired_enabled);

    let own_stop = persisted_copy("background-worker", "exec-live", false, BackgroundSubprocessStatus::Stopped);
    assert!(own_stop.adopt_cancellation_into(&mut record), "the launch's own stop is adopted");
    assert!(!record.desired_enabled, "adoption clears the cached desire");
}

/// A legacy stop is still adopted under its own canonical id.
///
/// A legacy ledger holds at most one row per canonical id, so that id is
/// the identity left once the exec session is absent. Refusing every
/// empty-session stop would strand a pre-session stop that a resumed
/// session must honour.
#[test]
fn a_legacy_stop_is_adopted_under_its_own_record_id() {
    let mut record = live_record("background-worker", "");
    let stop = persisted_copy("background-worker", "", false, BackgroundSubprocessStatus::Stopped);

    assert!(stop.adopt_cancellation_into(&mut record), "one legacy row is its own launch");
    assert!(!record.desired_enabled, "the legacy stop reaches the record it names");

    let mut other = live_record("background-other", "");
    let unrelated = persisted_copy("background-worker", "", false, BackgroundSubprocessStatus::Stopped);
    assert!(
        !unrelated.adopt_cancellation_into(&mut other),
        "another id names another record, whatever the shared blank session"
    );
    assert!(other.desired_enabled);
}

/// A legacy row's launch identity is the child session it names.
///
/// The canonical id names one record, but the ledger's row under it may be a
/// different legacy launch: only the child session can tell them apart. Two
/// blank exec sessions with different child sessions are therefore different
/// launches, and a stop of one must not govern the other's refresh.
#[test]
fn a_legacy_launch_identity_is_the_child_session_it_names() {
    let stopped = persisted_copy("background-worker", "", false, BackgroundSubprocessStatus::Stopped);
    let other_launch =
        persisted_copy_naming("background-worker", "", "session-other", true, BackgroundSubprocessStatus::Running);

    assert!(
        !stopped.shares_launch_identity_with(&other_launch),
        "different child sessions are different launches"
    );
    assert!(!stopped.cancellation_outranks(&other_launch), "a stop of one legacy launch must not govern another");

    let refresh = persisted_copy("background-worker", "", true, BackgroundSubprocessStatus::Running);
    assert!(stopped.shares_launch_identity_with(&refresh), "the same child session is one launch");
    assert!(stopped.cancellation_outranks(&refresh), "its stop outranks a refresh of the same child session");
}

/// A legacy stop is not adopted onto a record naming another child session.
///
/// The single ledger row under the canonical id is not evidence that the cached
/// record is the same launch; the child session each names is. A stop of another
/// child session is refused, and the record's own launch is still stopped.
#[test]
fn a_legacy_stop_is_not_adopted_across_child_sessions() {
    let mut record = live_record("background-worker", "");
    let other_launch =
        persisted_copy_naming("background-worker", "", "session-other", false, BackgroundSubprocessStatus::Stopped);
    assert!(!other_launch.adopt_cancellation_into(&mut record), "another child session is another launch");
    assert!(record.desired_enabled, "a refused adoption leaves the launch wanting to run");

    let own_stop = persisted_copy("background-worker", "", false, BackgroundSubprocessStatus::Stopped);
    assert!(own_stop.adopt_cancellation_into(&mut record), "the record's own child session is its launch");
    assert!(!record.desired_enabled, "the genuine legacy stop reaches the record it names");
}

fn persisted_copy(
    id: &str,
    exec_session_id: &str,
    desired_enabled: bool,
    status: BackgroundSubprocessStatus,
) -> PersistedBackgroundRecord {
    persisted_copy_naming(id, exec_session_id, &format!("session-{id}"), desired_enabled, status)
}

/// Builds a persisted copy that names an explicit child session.
fn persisted_copy_naming(
    id: &str,
    exec_session_id: &str,
    session_id: &str,
    desired_enabled: bool,
    status: BackgroundSubprocessStatus,
) -> PersistedBackgroundRecord {
    serde_json::from_value(json!({
        "id": id,
        "owner_session_id": null,
        "agent_name": "worker",
        "display_label": "worker",
        "description": "worker",
        "source": "builtin",
        "color": null,
        "session_id": session_id,
        "parent_session_id": "parent-session",
        "exec_session_id": exec_session_id,
        "desired_enabled": desired_enabled,
        "status": status.as_str(),
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": "2026-08-28T00:00:00Z",
        "started_at": null,
        "ended_at": null,
        "pid": null,
        "prompt": "work",
        "summary": null,
        "error": null,
        "archive_path": null,
        "transcript_path": null,
        "max_turns": null,
        "model_override": null,
        "reasoning_override": null,
        "restart_attempts": 0,
    }))
    .expect("persisted ledger copy")
}

fn live_record(id: &str, exec_session_id: &str) -> BackgroundRecord {
    BackgroundRecord {
        id: id.to_string(),
        owner_session_id: None,
        agent_name: "worker".to_string(),
        display_label: "worker".to_string(),
        description: "worker".to_string(),
        source: "builtin".to_string(),
        colour: None,
        session_id: format!("session-{id}"),
        parent_session_id: "parent-session".to_string(),
        exec_session_id: exec_session_id.to_string(),
        desired_enabled: true,
        status: BackgroundSubprocessStatus::Running,
        created_at: "2026-08-28T00:00:00Z".parse().expect("created_at"),
        updated_at: "2026-08-28T00:00:00Z".parse().expect("updated_at"),
        started_at: None,
        ended_at: None,
        pid: None,
        prompt: "work".to_string(),
        summary: None,
        error: None,
        archive_path: None,
        transcript_path: None,
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        restart_attempts: 0,
    }
}

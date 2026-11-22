//! Launch and exec-session identity generation.
//!
//! Declared by `run_tests/mod.rs`. Pins that every generated launch session id
//! and exec-session id is unique, which is what lets an archive and a close be
//! attributed to one launch.
use super::*;

/// Two launches of one record in one process never reuse an exec-session id.
#[test]
fn every_generated_exec_session_id_is_unique() {
    let first = background_exec_session_id("session-background-worker");
    let second = background_exec_session_id("session-background-worker");

    assert_ne!(first, second, "a repeated id would make one launch fail on the duplicate-session check");
    assert!(first.starts_with("exec-session-background-worker-"), "{first}");
}

/// Two launches in one millisecond never share a session id.
///
/// The timestamp component of the session id only has millisecond resolution,
/// and the archive attachment uses the session id to identify a launch. A
/// shared id would let the earlier run's archive attach to its replacement, so
/// the identity must carry a process-unique sequence as well.
#[test]
fn every_generated_launch_session_id_is_unique() {
    let base = "parent-session-background-worker-20260101T000000000Z";
    let first = background_launch_session_id(base);
    let second = background_launch_session_id(base);

    assert_ne!(first, second, "two launches in one millisecond must not share a session id");
    assert!(first.starts_with(base), "{first}");
}

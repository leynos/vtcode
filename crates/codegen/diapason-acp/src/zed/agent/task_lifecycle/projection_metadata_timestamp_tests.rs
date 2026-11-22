//! Start and end instants reported for every projected lifecycle state.
use super::{background_event_state, lody_task_session_update, subagent_event_state, task_meta};
use serde_json::json;

/// Every lifecycle state reports its own start and end instants, written as
/// literal epoch seconds rather than recomputed by the projection helper.
#[test]
fn every_lifecycle_state_reports_its_own_start_and_end_instants() {
    const CREATED_AT: &str = "2026-08-14T12:00:00Z";
    const STARTED_AT: &str = "2026-08-14T12:00:30Z";
    const ENDED_AT: &str = "2026-08-14T12:04:05Z";
    const CREATED_EPOCH_SECONDS: f64 = 1_786_708_800.0;
    const STARTED_EPOCH_SECONDS: f64 = 1_786_708_830.0;
    const ENDED_EPOCH_SECONDS: f64 = 1_786_709_045.0;

    // (record status, projected status, completion instant)
    let subagent_cases = [
        ("queued", "pending", None),
        ("running", "in_progress", None),
        ("waiting", "in_progress", None),
        // A completed child sent new input is queued again while still
        // carrying the previous run's completion instant; the wire contract
        // allows an end only for a terminal task.
        ("queued", "pending", Some(ENDED_AT)),
        ("running", "in_progress", Some(ENDED_AT)),
        ("waiting", "in_progress", Some(ENDED_AT)),
        ("completed", "completed", Some(ENDED_AT)),
        ("failed", "failed", Some(ENDED_AT)),
        ("closed", "failed", Some(ENDED_AT)),
    ];
    for (record_status, projected_status, completed_at) in subagent_cases {
        let event = subagent_event_state(record_status, CREATED_AT, completed_at);
        let update = lody_task_session_update(event, false).expect("a subagent task update");
        let meta = task_meta(&update);
        assert_eq!(meta["status"], projected_status, "a subagent {record_status} record projects its lifecycle status");
        assert_eq!(
            meta["startedAtEpochSeconds"],
            json!(CREATED_EPOCH_SECONDS),
            "a subagent {record_status} record starts at its creation instant"
        );
        let terminal = matches!(record_status, "completed" | "failed" | "closed");
        match (completed_at, terminal) {
            (Some(_), true) => assert_eq!(
                meta["endedAtEpochSeconds"],
                json!(ENDED_EPOCH_SECONDS),
                "a terminal subagent {record_status} record ends at its completion instant"
            ),
            _ => assert!(
                meta.get("endedAtEpochSeconds").is_none(),
                "an unfinished subagent {record_status} record carries no end instant"
            ),
        }
    }

    // (record status, projected status, start instant, end instant)
    let background_cases = [
        ("starting", "pending", None, None),
        ("running", "in_progress", Some(STARTED_AT), None),
        ("stopped", "completed", Some(STARTED_AT), Some(ENDED_AT)),
        ("error", "failed", Some(STARTED_AT), Some(ENDED_AT)),
    ];
    for (record_status, projected_status, started_at, ended_at) in background_cases {
        let event = background_event_state(record_status, CREATED_AT, started_at, ended_at);
        let update = lody_task_session_update(event, false).expect("a background task update");
        let meta = task_meta(&update);
        assert_eq!(
            meta["status"], projected_status,
            "a background {record_status} record projects its lifecycle status"
        );
        let expected_start = if started_at.is_some() {
            STARTED_EPOCH_SECONDS
        } else {
            CREATED_EPOCH_SECONDS
        };
        assert_eq!(
            meta["startedAtEpochSeconds"],
            json!(expected_start),
            "a background {record_status} record reports its recorded start, falling back to its creation"
        );
        match ended_at {
            Some(_) => assert_eq!(
                meta["endedAtEpochSeconds"],
                json!(ENDED_EPOCH_SECONDS),
                "a background {record_status} record ends at its recorded end instant"
            ),
            None => assert!(
                meta.get("endedAtEpochSeconds").is_none(),
                "a running background {record_status} record carries no end instant"
            ),
        }
    }
}

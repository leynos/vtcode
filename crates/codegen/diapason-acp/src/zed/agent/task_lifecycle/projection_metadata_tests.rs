//! Full Lody task-metadata projection for initial and update events.
use super::super::lody::lody_task_session_update;
use super::projection_tests::{background_event, subagent_event, task_meta};
use super::*;
use chrono::{DateTime, Utc};
use serde_json::json;

fn subagent_event_with(
    status: &str,
    summary: Option<&str>,
    error: Option<&str>,
    completed_at: Option<&str>,
) -> LifecycleUpdate {
    let mut event = subagent_event(status);
    let LifecycleUpdate::Subagent { task, .. } = &mut event else {
        panic!("subagent fixture must build a subagent event");
    };
    task.summary = summary.map(str::to_string);
    task.error = error.map(str::to_string);
    task.completed_at = completed_at.map(|value| value.parse().expect("RFC 3339 completion timestamp"));
    event
}

fn background_event_with(
    status: &str,
    summary: Option<&str>,
    error: Option<&str>,
    started_at: Option<&str>,
    ended_at: Option<&str>,
) -> LifecycleUpdate {
    let mut event = background_event(status);
    let LifecycleUpdate::BackgroundProcess { task, .. } = &mut event else {
        panic!("background fixture must build a background event");
    };
    task.summary = summary.map(str::to_string);
    task.error = error.map(str::to_string);
    task.started_at = started_at.map(|value| value.parse().expect("RFC 3339 start timestamp"));
    task.ended_at = ended_at.map(|value| value.parse().expect("RFC 3339 end timestamp"));
    event
}

fn subagent_event_state(status: &str, created_at: &str, completed_at: Option<&str>) -> LifecycleUpdate {
    let mut event = subagent_event(status);
    let LifecycleUpdate::Subagent { task, .. } = &mut event else {
        panic!("subagent fixture must build a subagent event");
    };
    task.created_at = created_at.parse().expect("RFC 3339 creation timestamp");
    task.completed_at = completed_at.map(|value| value.parse().expect("RFC 3339 completion timestamp"));
    event
}

fn background_event_state(
    status: &str,
    created_at: &str,
    started_at: Option<&str>,
    ended_at: Option<&str>,
) -> LifecycleUpdate {
    let mut event = background_event(status);
    let LifecycleUpdate::BackgroundProcess { task, .. } = &mut event else {
        panic!("background fixture must build a background event");
    };
    task.created_at = created_at.parse().expect("RFC 3339 creation timestamp");
    task.started_at = started_at.map(|value| value.parse().expect("RFC 3339 start timestamp"));
    task.ended_at = ended_at.map(|value| value.parse().expect("RFC 3339 end timestamp"));
    event
}

fn expected_epoch(value: &str) -> f64 {
    let timestamp: DateTime<Utc> = value.parse().expect("RFC 3339 timestamp");
    timestamp.timestamp_millis().max(0) as f64 / 1_000.0
}

/// Returns the single human-readable text block in a task's tool-call content.
fn content_text(content: &[acp::ToolCallContent]) -> String {
    let [acp::ToolCallContent::Content(content)] = content else {
        panic!("a task update carries exactly one text content block: {content:?}");
    };
    let acp::ContentBlock::Text(text) = &content.content else {
        panic!("a task update carries text content: {:?}", content.content);
    };
    text.text.clone()
}

/// Returns the text content of a projected task snapshot.
fn task_content_text(update: &acp::SessionUpdate) -> String {
    match update {
        acp::SessionUpdate::ToolCall(call) => content_text(&call.content),
        acp::SessionUpdate::ToolCallUpdate(update) => content_text(
            update
                .fields
                .content
                .as_deref()
                .expect("a task update always replaces its content"),
        ),
        other => panic!("expected a task-carrying update, got {other:?}"),
    }
}

#[test]
fn projects_complete_subagent_metadata_for_initial_and_update_events() {
    let event =
        subagent_event_with("completed", Some("Progress summary"), Some("tool failed"), Some("2026-08-14T12:02:00Z"));

    let initial = lody_task_session_update(event.clone(), false).expect("initial subagent task update");
    let acp::SessionUpdate::ToolCall(call) = &initial else {
        panic!("a first task snapshot must be a tool call");
    };
    assert_eq!(call.title, "Worker", "the tool-call title is the display label");
    assert_eq!(call.kind, acp::ToolKind::Think);
    assert_eq!(call.status, acp::ToolCallStatus::Completed);
    assert_eq!(task_content_text(&initial), "tool failed", "error content takes precedence over summary");

    let meta = task_meta(&initial);
    assert_eq!(meta["taskId"], "child-1");
    assert_eq!(meta["kind"], "subagent");
    assert_eq!(meta["status"], "completed");
    assert_eq!(meta["description"], "Implement one bounded task");
    assert_eq!(meta["actor"], "worker");
    assert_eq!(meta["startedAtEpochSeconds"], expected_epoch("2026-08-14T12:00:00Z"));
    assert_eq!(meta["endedAtEpochSeconds"], expected_epoch("2026-08-14T12:02:00Z"));
    assert_eq!(meta["summary"], "Progress summary");
    assert_eq!(meta["error"], "tool failed");

    let repeated = lody_task_session_update(event, true).expect("subagent task update");
    let acp::SessionUpdate::ToolCallUpdate(update) = &repeated else {
        panic!("a repeated task snapshot must be a tool-call update");
    };
    assert_eq!(update.fields.title.as_deref(), Some("Worker"));
    assert_eq!(update.fields.kind, Some(acp::ToolKind::Think));
    assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Completed));
    let content = update
        .fields
        .content
        .as_deref()
        .expect("a task update always replaces its content");
    assert_eq!(content_text(content), "tool failed");
    assert_eq!(task_meta(&repeated), meta, "the update must repeat the full task metadata");
}

#[test]
fn projects_complete_background_metadata_for_initial_and_update_events() {
    let event = background_event_with(
        "error",
        Some("Watching"),
        Some("process crashed"),
        Some("2026-08-14T12:00:30Z"),
        Some("2026-08-14T12:03:00Z"),
    );

    let initial = lody_task_session_update(event.clone(), false).expect("initial background task update");
    let acp::SessionUpdate::ToolCall(call) = &initial else {
        panic!("a first task snapshot must be a tool call");
    };
    assert_eq!(call.title, "Watcher");
    assert_eq!(call.status, acp::ToolCallStatus::Failed);
    assert_eq!(task_content_text(&initial), "process crashed");

    let meta = task_meta(&initial);
    assert_eq!(meta["taskId"], "background-1");
    assert_eq!(meta["kind"], "background");
    assert_eq!(meta["status"], "failed");
    assert_eq!(meta["description"], "Watch the workspace");
    assert_eq!(meta["actor"], "watcher");
    assert_eq!(
        meta["startedAtEpochSeconds"],
        expected_epoch("2026-08-14T12:00:30Z"),
        "a started background task reports its start, not its creation"
    );
    assert_eq!(meta["endedAtEpochSeconds"], expected_epoch("2026-08-14T12:03:00Z"));
    assert_eq!(meta["summary"], "Watching");
    assert_eq!(meta["error"], "process crashed");
    assert!(meta.get("pid").is_none(), "the task metadata must not expose the process id");
    assert!(
        meta.get("sessionId").is_none() && meta.get("transcriptPath").is_none(),
        "the task metadata must not expose internal identifiers"
    );

    let repeated = lody_task_session_update(event, true).expect("background task update");
    let acp::SessionUpdate::ToolCallUpdate(update) = &repeated else {
        panic!("a repeated task snapshot must be a tool-call update");
    };
    assert_eq!(update.fields.title.as_deref(), Some("Watcher"));
    assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Failed));
    let content = update
        .fields
        .content
        .as_deref()
        .expect("a task update always replaces its content");
    assert_eq!(content_text(content), "process crashed");
    assert_eq!(task_meta(&repeated), meta);
}

#[test]
fn background_task_without_a_start_time_reports_its_creation() {
    let event = background_event_with("starting", None, None, None, None);
    let update = lody_task_session_update(event, false).expect("initial background task update");
    assert_eq!(
        task_meta(&update)["startedAtEpochSeconds"],
        expected_epoch("2026-08-14T12:00:00Z"),
        "a task that has not started yet reports its creation instant"
    );
    assert!(task_meta(&update).get("endedAtEpochSeconds").is_none(), "an active task has no end instant");
    assert_eq!(task_meta(&update)["status"], "pending");
}

#[test]
fn task_content_prefers_error_then_summary_then_description() {
    let subagent_cases = [
        (Some("boom"), Some("summary"), "boom"),
        (None, Some("summary"), "summary"),
        (None, None, "Implement one bounded task"),
    ];
    let background_cases = [
        (Some("crashed"), Some("watching"), "crashed"),
        (None, Some("watching"), "watching"),
        (None, None, "Watch the workspace"),
    ];

    // Cover both emission forms: the initial tool call and the later update.
    for previously_emitted in [false, true] {
        for (error, summary, expected) in subagent_cases {
            let event = subagent_event_with("failed", summary, error, None);
            let update = lody_task_session_update(event, previously_emitted).expect("subagent task update");
            assert_eq!(
                task_content_text(&update),
                expected,
                "subagent content precedence (previously_emitted={previously_emitted})"
            );
        }

        for (error, summary, expected) in background_cases {
            let event = background_event_with("error", summary, error, None, None);
            let update = lody_task_session_update(event, previously_emitted).expect("background task update");
            assert_eq!(
                task_content_text(&update),
                expected,
                "background content precedence (previously_emitted={previously_emitted})"
            );
        }
    }
}

#[test]
fn pre_epoch_timestamps_are_clamped_to_zero() {
    for previously_emitted in [false, true] {
        // Subagent: both the creation and completion instants are pre-epoch.
        let update = lody_task_session_update(
            subagent_event_state("running", "1960-01-01T00:00:00Z", Some("1960-01-01T00:00:00Z")),
            previously_emitted,
        )
        .expect("subagent task update");
        assert_eq!(
            task_meta(&update)["startedAtEpochSeconds"],
            json!(0.0),
            "a timestamp before the Unix epoch must clamp to zero, not report a negative instant"
        );
        assert_eq!(
            task_meta(&update)["endedAtEpochSeconds"],
            json!(0.0),
            "a pre-epoch completion must clamp to zero (previously_emitted={previously_emitted})"
        );

        // Background: creation, start and end are all pre-epoch.
        let update = lody_task_session_update(
            background_event_state(
                "starting",
                "1960-01-01T00:00:00Z",
                Some("1960-01-01T00:00:00Z"),
                Some("1960-01-01T00:00:00Z"),
            ),
            previously_emitted,
        )
        .expect("background task update");
        assert_eq!(
            task_meta(&update)["startedAtEpochSeconds"],
            json!(0.0),
            "a pre-epoch background start must clamp to zero (previously_emitted={previously_emitted})"
        );
        assert_eq!(
            task_meta(&update)["endedAtEpochSeconds"],
            json!(0.0),
            "a pre-epoch background end must clamp to zero (previously_emitted={previously_emitted})"
        );

        // Control: a real post-epoch end still projects its true epoch, so the
        // test cannot pass by clamping every timestamp.
        let update = lody_task_session_update(
            subagent_event_state("completed", "2026-08-14T12:00:00Z", Some("2026-08-14T12:02:00Z")),
            previously_emitted,
        )
        .expect("subagent control task update");
        assert_eq!(
            task_meta(&update)["endedAtEpochSeconds"],
            expected_epoch("2026-08-14T12:02:00Z"),
            "a post-epoch completion must project its true epoch (previously_emitted={previously_emitted})"
        );

        let update = lody_task_session_update(
            background_event_state(
                "stopped",
                "2026-08-14T12:00:00Z",
                Some("2026-08-14T12:00:30Z"),
                Some("2026-08-14T12:03:00Z"),
            ),
            previously_emitted,
        )
        .expect("background control task update");
        assert_eq!(
            task_meta(&update)["endedAtEpochSeconds"],
            expected_epoch("2026-08-14T12:03:00Z"),
            "a post-epoch background end must project its true epoch (previously_emitted={previously_emitted})"
        );
    }
}

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
        match completed_at {
            Some(_) => assert_eq!(
                meta["endedAtEpochSeconds"],
                json!(ENDED_EPOCH_SECONDS),
                "a subagent {record_status} record ends at its completion instant"
            ),
            None => assert!(
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

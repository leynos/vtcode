//! Standard ACP lifecycle projection and transport regressions.
use super::super::lody::lody_task_session_update;
use super::*;
use chrono::{DateTime, Utc};
use proptest::prelude::*;
use serde_json::{Value, json};

pub(super) fn subagent_event(status: &str) -> LifecycleUpdate {
    subagent_event_for("parent-session", status)
}

pub(super) fn subagent_event_for(parent_session_id: &str, status: &str) -> LifecycleUpdate {
    LifecycleUpdate::Subagent {
        parent_session_id: parent_session_id.to_string(),
        task: serde_json::from_value(json!({
            "id": "child-1",
            "session_id": "child-session",
            "parent_thread_id": "parent-session",
            "agent_name": "worker",
            "display_label": "Worker",
            "description": "Implement one bounded task",
            "source": "builtin",
            "status": status,
            "background": false,
            "depth": 1,
            "created_at": "2026-08-14T12:00:00Z",
            "updated_at": "2026-08-14T12:01:00Z",
            "summary": "Progress summary"
        }))
        .expect("valid subagent status fixture"),
    }
}

pub(super) fn background_event(status: &str) -> LifecycleUpdate {
    LifecycleUpdate::BackgroundProcess {
        parent_session_id: "parent-session".to_string(),
        task: serde_json::from_value(json!({
            "id": "background-1",
            "session_id": "background-session",
            "exec_session_id": "exec-background-1",
            "agent_name": "watcher",
            "display_label": "Watcher",
            "description": "Watch the workspace",
            "source": "workspace",
            "status": status,
            "desired_enabled": true,
            "created_at": "2026-08-14T12:00:00Z",
            "updated_at": "2026-08-14T12:01:00Z",
            "pid": 1234,
            "summary": "Watching"
        }))
        .expect("valid background status fixture"),
    }
}

pub(super) fn task_meta(update: &acp::SessionUpdate) -> &Value {
    let meta = match update {
        acp::SessionUpdate::ToolCall(call) => call.meta.as_ref(),
        acp::SessionUpdate::ToolCallUpdate(update) => update.meta.as_ref(),
        other => panic!("expected task-carrying tool update, got {other:?}"),
    }
    .expect("task update metadata");
    &meta["lody"]["task"]
}

pub(super) fn with_updated_at(mut event: LifecycleUpdate, value: &str) -> LifecycleUpdate {
    let timestamp: DateTime<Utc> = value.parse().expect("RFC 3339 transition timestamp");
    match &mut event {
        LifecycleUpdate::Subagent { task, .. } => task.updated_at = timestamp,
        LifecycleUpdate::BackgroundProcess { task, .. } => task.updated_at = timestamp,
    }
    event
}

pub(super) fn with_task_id(mut event: LifecycleUpdate, task_id: String) -> LifecycleUpdate {
    match &mut event {
        LifecycleUpdate::Subagent { task, .. } => task.id = task_id,
        LifecycleUpdate::BackgroundProcess { task, .. } => task.id = task_id,
    }
    event
}

#[test]
fn maps_worker_and_background_states_to_lody_task_metadata() {
    let cases = [
        (subagent_event("queued"), "subagent", "pending"),
        (subagent_event("running"), "subagent", "in_progress"),
        (subagent_event("waiting"), "subagent", "in_progress"),
        (subagent_event("completed"), "subagent", "completed"),
        (subagent_event("failed"), "subagent", "failed"),
        (subagent_event("closed"), "subagent", "failed"),
        (background_event("starting"), "background", "pending"),
        (background_event("running"), "background", "in_progress"),
        (background_event("stopped"), "background", "completed"),
        (background_event("error"), "background", "failed"),
    ];

    for (event, kind, status) in cases {
        let update = lody_task_session_update(event, false).expect("valid Lody task update");
        let acp::SessionUpdate::ToolCall(call) = &update else {
            panic!("first task snapshot must be a tool call");
        };
        let task_id = task_meta(&update)["taskId"].as_str().expect("task ID string");
        assert_eq!(call.tool_call_id.0.as_ref(), format!("task:{task_id}"));
        assert_eq!(call.kind, acp::ToolKind::Think);
        assert_eq!(task_meta(&update)["version"], 1);
        assert_eq!(task_meta(&update)["kind"], kind);
        assert_eq!(task_meta(&update)["status"], status);
        assert!(task_meta(&update).get("sessionId").is_none());
        assert!(task_meta(&update).get("pid").is_none());
        assert!(task_meta(&update).get("transcriptPath").is_none());
    }
}

fn lifecycle_ladder(kind_index: usize) -> (&'static [&'static str], &'static [u8]) {
    const SUBAGENT: &[&str] = &["queued", "running", "waiting", "completed", "failed", "closed"];
    const SUBAGENT_ORDINALS: &[u8] = &[0, 1, 1, 2, 2, 2];
    const BACKGROUND: &[&str] = &["starting", "running", "stopped", "error"];
    const BACKGROUND_ORDINALS: &[u8] = &[0, 1, 2, 2];
    if kind_index == 0 {
        (SUBAGENT, SUBAGENT_ORDINALS)
    } else {
        (BACKGROUND, BACKGROUND_ORDINALS)
    }
}

fn projected_ordinal(status: &str) -> u8 {
    match status {
        "pending" => 0,
        "in_progress" => 1,
        "completed" | "failed" => 2,
        other => panic!("unexpected projected task status {other}"),
    }
}

proptest! {
    /// A task's lifecycle walk is a non-decreasing sequence of states. Whatever
    /// its length, projection must keep one stable tool-call ID and must never
    /// report a terminal state as pending or in progress again.
    #[test]
    fn lifecycle_sequences_keep_stable_ids_and_monotonic_terminality(
        kind_index in 0usize..2,
        steps in prop::collection::vec(0usize..8, 1..=32),
    ) {
        let (ladder, ordinals) = lifecycle_ladder(kind_index);
        // The two fixtures use different ids; the kind selects which.
        let expected_task_id = if kind_index == 0 { "child-1" } else { "background-1" };
        let expected_call_id = format!("task:{expected_task_id}");
        let mut walked = steps.into_iter().map(|step| step % ladder.len()).collect::<Vec<_>>();
        walked.sort_unstable();

        let mut last_ordinal = 0_u8;
        for (index, status_index) in walked.iter().copied().enumerate() {
            let status = ladder[status_index];
            let event = if kind_index == 0 { subagent_event(status) } else { background_event(status) };
            let update = lody_task_session_update(event, index > 0).expect("valid lifecycle projection");
            let task_id = match &update {
                acp::SessionUpdate::ToolCall(call) => {
                    prop_assert_eq!(index, 0, "only the first snapshot may create the tool call");
                    prop_assert_eq!(call.tool_call_id.0.as_ref(), expected_call_id.as_str());
                    task_meta(&update)["taskId"].as_str().expect("task ID")
                }
                acp::SessionUpdate::ToolCallUpdate(call_update) => {
                    prop_assert!(index > 0, "later snapshots must update the existing tool call");
                    prop_assert_eq!(call_update.tool_call_id.0.as_ref(), expected_call_id.as_str());
                    task_meta(&update)["taskId"].as_str().expect("task ID")
                }
                other => {
                    prop_assert!(false, "unexpected lifecycle projection {other:?}");
                    return Ok(());
                }
            };
            prop_assert_eq!(task_id, expected_task_id);
            let ordinal = projected_ordinal(task_meta(&update)["status"].as_str().expect("projected status"));
            prop_assert_eq!(ordinal, ordinals[status_index], "status {} must map to its ladder rung", status);
            prop_assert!(ordinal >= last_ordinal, "terminality must never regress for status {}", status);
            last_ordinal = ordinal;
        }
    }

    #[test]
    fn task_updates_keep_stable_ids_across_statuses(
        task_id in "[A-Za-z0-9_-]{1,64}",
        status_index in 0_usize..10,
    ) {
        let statuses = [
            "queued", "running", "waiting", "completed", "failed", "closed",
            "starting", "running", "stopped", "error",
        ];
        let event = if status_index < 6 {
            subagent_event(statuses[status_index])
        } else {
            background_event(statuses[status_index])
        };
        let event = with_task_id(event, task_id.clone());

        let initial = lody_task_session_update(event.clone(), false).expect("valid initial task update");
        let subsequent = lody_task_session_update(event, true).expect("valid subsequent task update");

        let acp::SessionUpdate::ToolCall(initial) = initial else {
            prop_assert!(false, "initial snapshot must create a tool call");
            return Ok(());
        };
        let acp::SessionUpdate::ToolCallUpdate(subsequent) = subsequent else {
            prop_assert!(false, "subsequent snapshot must update the tool call");
            return Ok(());
        };
        let expected_id = format!("task:{task_id}");
        prop_assert_eq!(initial.tool_call_id.0.as_ref(), expected_id.as_str());
        prop_assert_eq!(subsequent.tool_call_id.0.as_ref(), expected_id.as_str());
        let initial_task_id = initial.meta.as_ref().expect("initial meta")["lody"]["task"]["taskId"]
            .as_str()
            .expect("initial task ID");
        let subsequent_task_id = subsequent.meta.as_ref().expect("subsequent meta")["lody"]["task"]["taskId"]
            .as_str()
            .expect("subsequent task ID");
        prop_assert_eq!(initial_task_id, task_id.as_str());
        prop_assert_eq!(subsequent_task_id, task_id.as_str());
    }
}

#[test]
fn stale_initial_snapshot_is_rejected_after_a_terminal_transition() {
    let mut ordering = LifecycleOrdering::default();
    let running = with_updated_at(subagent_event("running"), "2026-08-14T12:01:00Z");
    let completed = with_updated_at(subagent_event("completed"), "2026-08-14T12:05:00Z");

    assert!(ordering.admit(&running), "the first snapshot must be forwarded");
    assert!(ordering.admit(&completed), "a newer terminal transition must be forwarded");
    assert!(
        !ordering.admit(&running),
        "an initial snapshot captured before the terminal transition must not roll the task back"
    );

    // A transition the controller really recorded after settlement still wins.
    let resumed = with_updated_at(subagent_event("running"), "2026-08-14T12:06:00Z");
    assert!(ordering.admit(&resumed), "newer lifecycle transitions must still be forwarded");
}

#[test]
fn equal_timestamp_revisions_never_regress_terminality() {
    let mut ordering = LifecycleOrdering::default();
    let completed = with_updated_at(subagent_event("completed"), "2026-08-14T12:05:00Z");
    assert!(ordering.admit(&completed));

    let same_instant_running = with_updated_at(subagent_event("running"), "2026-08-14T12:05:00Z");
    assert!(
        !ordering.admit(&same_instant_running),
        "a coarsened clock must not let a terminal task regress to running"
    );

    let mut background = LifecycleOrdering::default();
    let running = with_updated_at(background_event("running"), "2026-08-14T12:01:00Z");
    let stopped = with_updated_at(background_event("stopped"), "2026-08-14T12:05:00Z");
    assert!(background.admit(&running));
    assert!(background.admit(&stopped));
    assert!(
        !background.admit(&with_updated_at(background_event("running"), "2026-08-14T12:01:00Z")),
        "a stale background snapshot must not revive a stopped task"
    );
}

// ─── Full metadata projection ───────────────────────────────────────────────

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

fn subagent_event_at(created_at: &str) -> LifecycleUpdate {
    let mut event = subagent_event("running");
    let LifecycleUpdate::Subagent { task, .. } = &mut event else {
        panic!("subagent fixture must build a subagent event");
    };
    task.created_at = created_at.parse().expect("RFC 3339 creation timestamp");
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
    for (error, summary, expected) in subagent_cases {
        let event = subagent_event_with("failed", summary, error, None);
        let update = lody_task_session_update(event, false).expect("subagent task update");
        assert_eq!(task_content_text(&update), expected, "subagent content precedence");
    }

    let background_cases = [
        (Some("crashed"), Some("watching"), "crashed"),
        (None, Some("watching"), "watching"),
        (None, None, "Watch the workspace"),
    ];
    for (error, summary, expected) in background_cases {
        let event = background_event_with("error", summary, error, None, None);
        let update = lody_task_session_update(event, false).expect("background task update");
        assert_eq!(task_content_text(&update), expected, "background content precedence");
    }
}

#[test]
fn pre_epoch_timestamps_are_clamped_to_zero() {
    let update =
        lody_task_session_update(subagent_event_at("1960-01-01T00:00:00Z"), false).expect("subagent task update");
    assert_eq!(
        task_meta(&update)["startedAtEpochSeconds"],
        json!(0.0),
        "a timestamp before the Unix epoch must clamp to zero, not report a negative instant"
    );
}

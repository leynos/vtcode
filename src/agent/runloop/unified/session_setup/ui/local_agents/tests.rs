//! Tests for background subprocess rows in the local-agent sidebar.
use super::*;
use chrono::{DateTime, TimeZone, Utc};
use std::path::PathBuf;

fn timestamp(second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, second)
        .single()
        .expect("valid timestamp")
}

fn background_entry(status: BackgroundSubprocessStatus, updated_at: DateTime<Utc>) -> BackgroundSubprocessEntry {
    BackgroundSubprocessEntry {
        id: "background-1".to_string(),
        owner_session_id: None,
        session_id: "session-1".to_string(),
        exec_session_id: String::new(),
        agent_name: "default".to_string(),
        display_label: "default".to_string(),
        description: "Default agent".to_string(),
        source: "builtin".to_string(),
        colour: None,
        status,
        desired_enabled: true,
        created_at: updated_at,
        updated_at,
        started_at: None,
        ended_at: None,
        pid: None,
        summary: None,
        error: None,
        archive_path: None,
        transcript_path: None,
    }
}

#[test]
fn background_row_visibility_uses_the_snapshot_entry() {
    let refreshed = background_entry(BackgroundSubprocessStatus::Running, timestamp(0));
    let snapshot = BackgroundSubprocessSnapshot {
        entry: background_entry(BackgroundSubprocessStatus::Stopped, timestamp(1)),
        preview: String::new(),
    };

    assert!(
        background_local_agent_row(refreshed, Some(&snapshot)).is_none(),
        "a row must not survive when the observation used for rendering says it stopped"
    );
}

#[test]
fn background_row_renders_metadata_from_the_snapshot_entry() {
    let refreshed = background_entry(BackgroundSubprocessStatus::Running, timestamp(0));
    let mut observed = background_entry(BackgroundSubprocessStatus::Running, timestamp(1));
    observed.summary = Some("Observed summary".to_string());
    observed.transcript_path = Some(PathBuf::from("/tmp/observed.jsonl"));
    let snapshot = BackgroundSubprocessSnapshot {
        entry: observed,
        preview: "observed preview".to_string(),
    };

    let (updated_at, row) = background_local_agent_row(refreshed, Some(&snapshot)).expect("row should be visible");

    assert_eq!(updated_at, timestamp(1));
    assert_eq!(row.status, "running");
    assert_eq!(row.summary.as_deref(), Some("Observed summary"));
    assert_eq!(row.preview, "observed preview");
    assert_eq!(row.transcript_path, Some(PathBuf::from("/tmp/observed.jsonl")));
}

#[test]
fn background_row_without_a_snapshot_uses_the_refreshed_entry_placeholder() {
    let refreshed = background_entry(BackgroundSubprocessStatus::Running, timestamp(0));

    let (updated_at, row) = background_local_agent_row(refreshed, None).expect("row should be visible");

    assert_eq!(updated_at, timestamp(0));
    assert_eq!(row.status, "running");
    assert_eq!(row.preview, "Subprocess is running; waiting for the next transcript update.");
    assert_eq!(row.summary.as_deref(), Some("Running; waiting for transcript output."));
}

#[test]
fn background_row_without_a_snapshot_still_applies_visibility() {
    let refreshed = background_entry(BackgroundSubprocessStatus::Stopped, timestamp(0));

    assert!(background_local_agent_row(refreshed, None).is_none());
}
